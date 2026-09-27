# SIMesh

A LoRa testbed, run in real time or in virtual time. Stations are real
firmware, built as Linux processes, each on its own loopback address; below
their radio driver sits a model of an SX1262 on a virtual SPI (serial
peripheral interface) bus, and between the models sits one medium, the
ether, that decides who hears what from a table of every pair's path loss.
A map in the browser places the nodes on real ground or on synthetic ground
and shows every frame on the air.

```
browser ── localhost:8800 ──► front.py ─┬─► simd (lora) ─┬─ ether        (UDP, in-process)
                                        │                ├─ stations     (firmware processes, one pty each)
                                        │                └─ proxy        <station>.lora.sim.localhost ─► the station's :80
                                        ├─► simd (supe)  …
                                        ├─► simesh.runner   a script's main, driving one simulation
                                        └─► planner-web  one per ground-data pack in use
```

One simulation is one simd; the front runs several behind port 8800, keeps
their registry, computes their loss tables, runs scripts against them and
starts the planner that supplies real ground. Around them are the things
SIMesh keeps, each on its own because each changes on its own:

| | What it is | Where |
|---|---|---|
| a **device** | one station build: its executable, its `/fixed` tree, its tools | `devices/<catalogue>/<package>/`, `devices/local/<name>.yaml` |
| **geodata** | the ground: a planner pack, or synthetic ground at 0°, 0° | `testbed/geodata/<name>.yaml` |
| a **nodeset** | which nodes stand where, what device each runs, its role and radio, and the offsets | `testbed/nodesets/<name>.yaml` |
| a **loss table** | every ordered pair's path loss, derived from geodata and a nodeset | `testbed/losses/…`, a cache |
| a **script** | Python against the simesh library: a station's setup, a driver | `testbed/scripts/<name>.py` |
| a **run** | one simulation's output: its record, logs and state | `testbed/runs/<name>/` |
| a **snapshot** | a moment of a run, state and all, to start another from | `testbed/snapshots/<name>/` |

| Piece | Where |
|---|---|
| the launcher | `simesh`, and its image, `Dockerfile` |
| the medium | [`ether/`](ether/README.md) |
| the chip model, as a C library a station links | `radio/` |
| the front, simd, the stores, the library, the page, the analysis tools | `testbed/` |
| what a station process is promised and owes | [STATION.md](STATION.md) |
| the device file, a prebuilt station | [NODE.md](NODE.md) |
| the loss table's file format | [LOSSTABLE.md](LOSSTABLE.md) |

It is the firmware under test, built for a different target — not an emulator.
[INTERNALS.md](INTERNALS.md) says how it works and why it is built this way.

## Getting started

SIMesh needs no firmware tree: it runs prebuilt stations it fetches itself.

On **Linux** it runs natively, and needs `python3` with `aiohttp` and
`pyyaml` (Debian and Ubuntu: `python3-aiohttp python3-yaml`), `node` and
`npm` for the page, and `cmake` with a C and C++ compiler for the chip
library; `cargo` too for real ground (below). **Anywhere else** it needs
only `docker`: `simesh` builds its own small image on first use (a few
minutes, once) and runs itself inside it, with port 8800 published.

**1. Clone it.** The directory you clone into is the one `simesh` mounts
into its image, so a workspace put beside SIMesh later is where a local
device expects it:

```sh
mkdir mesh && cd mesh
git clone https://github.com/reticulous/SIMesh.git
```

**2. Build** the page, the chip library and the planner (SIMesh's own, in
`planner/`; without cargo it is left out, SIMesh says so, and synthetic
ground works):

```sh
SIMesh/simesh build
```

**3. Start it:**

```sh
SIMesh/simesh
```

It starts the front and opens `http://localhost:8800/`. The terminal is the
testbed's: Ctrl-C there stops every simulation and everything they started.

**4. Stations.** A station is a device file: the firmware built for Linux
and a `node.yaml` saying how to run it, published in a build catalogue beside
the board images. On start the front fetches the newest three of the `stable`
and `dev` catalogues' devices for this machine's architecture (`aarch64` or
`x86_64`) into `SIMesh/devices/`, and copies in those of every catalogue in
a `builds/` directory beside SIMesh; the Devices tab shows them.

**5. A first simulation.** On the **Geodata** tab click `plain-27`; the
**Nodes** tab then lists the nodesets on it. Click `smoke7`: seven nodes on
flat synthetic ground. On the **Simulations** tab
pick `smoke7` as the setup script and press **Run from current world**. The
Nodes tab comes back attached to the new simulation, and its stations come up
over the next minute; rings on the map are frames on the air.

**While `stable` has no `hw-simesh` device** for your architecture, the
start is refused with the command that would fetch one. Select every node,
set its device to `dev` in the editor (or a build of your own, [The developer
loop](#the-developer-loop-with-a-workspace)), and start again; the change is
saved on the way.

**6. Play with it.**

- **Click a station** for the editor: how it is doing, its **Console** (its
  serial console) and **Web UI** (its own web interface, exactly as a board
  serves it; log in as `admin`, password `admin`, which the `smoke7` script
  sets), and every setting it has.
- **Right-click the map ▸ New node here** puts down a station. It comes up
  and is set up on its own.
- **Drag a station** and its links change once its row of the loss table is
  recomputed.
- **Right-click a station ▸ Run command…** types one line at the selected
  stations: `lora 0 a` makes them announce, `rnpath -s` says what paths each
  knows.
- **⋯ ▸ Save snapshot as…** on the simulation's row of the Simulations tab
  keeps the network and everything its stations have become, and **⋯ ▸ Load
  snapshot into it…** brings one back.

What to read next: [Devices](#devices), [Geodata](#geodata),
[Nodesets](#nodesets) and [Scripts](#scripts) for making your own network,
[The page](#the-page) for doing it on the map, [Running
simulations](#running-simulations) for the time modes.

## Station kinds

Stations of different firmwares share one ether and one map. Each firmware is
a **kind**: how the testbed talks to it (`testbed/kinds/`). A device's
`node.yaml` names its kind. Two exist:

| Kind | The firmware | Up when | A line is | Web UI |
|---|---|---|---|---|
| `reticulous` | Reticulous, built for `spangap/hw-linux` | it answers a framed RPC (remote procedure call) frame on its console (after printing the marker, or to one blind probe), and `s.sys.reset_reason` reads, which its boot writes once every service has initialised | a CLI (command line) command, one framed RPC frame over its console pty (pseudo-terminal) | port 80 |
| `berlinmesh` | "Sergeyculum", the Rust Reticulum stack at [git.emcomm.cc/berlinmesh/reticulum](https://git.emcomm.cc/berlinmesh/reticulum), as its `fw/simesh` target | its `kiss` pty answers `rncfg detect` | `rncfg` without program and port: `name set {name}` runs `rncfg name <dir>/kiss set <name>` | none |

Sergeyculum is a working name; the project calls itself `reticulum` and the
kind is named after its repository.

A kind reports each station's **role** in the mesh, read live from the
station: `transport`, `router` or `repeater` for one that carries others'
traffic, `client` for one that does not.

A kind also says **intents** in its own lines, so that a nodeset's declared
settings and a script mean the same thing on any firmware:

| Intent | `reticulous` | `berlinmesh` |
|---|---|---|
| `name` | `hostname {name}` | `name set {name}` |
| `role` | `set s.rnsd.transport_enabled 1` or `0` | `transport on` or `off` (kept in RAM only, so said again at every boot) |
| `radio` | `lora 0 freq`, `sf`, `bw`, `cr`, `txp`, `sync`, `preamble` for each figure given, then `lora up` | `set --freq-hz --sf --bw-hz --cr --txpower-dbm` for the figures given; sync word and preamble are the firmware's own |
| `announce` | `lora 0 a` | `announce now` |
| `message` | `lxmf send <dest> <text>` | `send <dest> <text>` |
| `path` | `rnpath -j <dest>` | — |
| `peer_tcp` | `tcp peer add <addr>:<port>` | — |

An intent a kind has no line for is refused, naming the kind and the verb. A
station's LXMF delivery address, which `message` and `path` need of the
other end, is its `lxmf` listing's on `reticulous` and the `lxmf.delivery`
line of `rncfg addr` on `berlinmesh`.

## Devices

A device is one station build for one architecture
([NODE.md](NODE.md) is the file's spec): a zip holding its executable,
whatever else it needs, and a `node.yaml` saying what those are, what the
page calls it (`name`) and what hardware it plays (`stands_for`, shown as
"virtual ESP32"). Build catalogues publish them beside the board images as
`<project>_hw-simesh-<arch>_<stamp>.zip`.

The front fetches them itself when it starts: the newest three per catalogue
of `stable` and `dev` from the web, then of every catalogue directory (one
holding an `index.html`) in `builds/` beside SIMesh, which is where a
workspace's `make-builds` leaves them. A workspace catalogue joins the one of
its name, so a fresh local `dev` build is `dev`'s newest; one called `local`
becomes `builds-local`. The `builds/` directories are looked at again
whenever the Devices tab lists, and **Refresh** there does the web too. From
a shell:

```sh
simesh devices refresh                       # the web's stable and dev, then builds/
simesh devices refresh dev                   # one catalogue, by name
simesh devices refresh https://…/builds/dev/ # a catalogue at a URL
simesh devices refresh ../builds/rop         # a local catalogue directory
simesh devices import mine.zip               # any device zip, into `imported`
simesh devices list                          # every device there is
simesh devices resolve stable                # what a `device:` value runs, as JSON
```

A bare name is fetched from `$SIMESH_CATALOGUES<name>/`, by default
`https://reticulous.net/builds/<name>/`. Devices land in
`SIMesh/devices/<catalogue>/<package>/`, whole or not at all; the three
newest per entry per catalogue are kept, and the Devices tab shows the three
newest of each catalogue. **Import** on the Devices tab, or `simesh devices
import`, takes a zip of any name into the catalogue `imported`, named from
its `node.yaml`; it must be built for this machine.

A **local device** is `devices/local/<name>.yaml`: a `node.yaml` that is not
in an archive, its `elf`, `fixed` and `tools` paths relative to the file and
free to point anywhere, for a build of one's own run in place. Two come with
SIMesh: `reticulous-workspace`, the workspace's `reticulous/esp-idf/build.linux`,
and `berlinmesh-workspace`, Sergeyculum's `fw/simesh` with its `rncfg`.

A device's executable is native code linked against its builder's C
library, C++ runtime, zlib and libbsd, so it runs on a machine of its
architecture with those present; SIMesh's image is Ubuntu 24.04 of the
host's own architecture for that reason.

**What a node's `device:` names**, and the build field on the page,
`simesh new --build` and `simd --build`:

| Value | Runs |
|---|---|
| `stable`, `dev`, `imported`, or any fetched catalogue's name | the newest device from it for this machine |
| `<catalogue>/<package>`, as `dev/reticulous_hw-simesh-aarch64_20260925035045` | that device; the package's bare name will do while only one catalogue has it |
| a stamp, `20260925035045` | the device with that stamp |
| a local device's name, `reticulous-workspace` | that build, in place |
| a package directory, or a workspace's `build.linux` | that one |

A node naming no device runs `stable`. A build given to a simulation runs in
place of every device of that build's kind; a simulation records the device
each node resolved to, and so does a snapshot.

## Geodata

Geodata is the ground nodes stand on, one of two kinds:

```yaml
# testbed/geodata/plain-27.yaml: synthetic ground, a flat plane at 0°, 0°
synthetic:
  terrain: flat
  exponent: 2.7                     # log-distance path-loss exponent
  extent_m: 150000                  # the square it covers, centred on 0°, 0°
```

```yaml
# testbed/geodata/berlin-city.yaml: a pack, by path from this file
pack: ../../packs/berlin-city
```

**Synthetic ground** lies at 0°, 0°, and its degrees are metres by one fixed
rule on both axes, a nautical mile to the minute of arc: `x = lon · 60 · 1852
m`, `y = lat · 60 · 1852 m`. So a node's position is still latitude and
longitude, the map shows either, and a nodeset made on one synthetic ground
stands on any other. A pair's loss on flat terrain is log-distance,
`FSPL(1 m, f) + 10·n·log10(d)` (FSPL: free-space path loss); `n` is 2 for free
space, 2.7 suburban, and higher numbers bring the neighbourhoods in closer.
`plain-27` and `plain-35` come with SIMesh.

**A pack** is ground data compiled from public sources: terrain, clutter,
buildings, roads and places in a UTM (Universal Transverse Mercator) zone.
Packs live in `SIMesh/packs/<name>/` (not committed). A pack's geodata says
nothing its pack does not: its extent, projection and layers come from the
pack's `manifest.json`. On a pack, a pair's loss is ITU-R (International
Telecommunication Union, radio sector) Recommendation P.1812-8 over the real
profile, as SIMesh's planner computes it, and the map draws the pack's
ground, roads and buildings. **Import** on the Geodata tab takes a zip of a
pack directory (its `manifest.json` at the top or inside one directory),
expands it into `packs/<name>/` and writes the geodata that names it.
Building a pack from its sources is not in SIMesh yet: packs are still
compiled by Sergey's planner (`planner pack build`) and imported.

A node's height is always above the ground under it, so the same nodes on
other ground rise and fall with it.

**The planner.** A pack needs `planner-web`, SIMesh's own ground and
propagation server, built from the Rust workspace in `SIMesh/planner/` (the
crates came from Sergey's planner, and keep their names). SIMesh runs it as
a **sidecar**: one per pack in use, started by the front on a free loopback
port when the first simulation or page opens geodata on that pack, and
stopped when the last one lets go. The page reaches it as
`/planner/<geodata>/…` on port 8800. `simesh build planner` builds it with
cargo (in SIMesh's image on a machine that is not Linux). Not built, a pack
is refused with the sentence saying so, and synthetic ground works.

Geodata names no node, and a nodeset names no geodata. The medium's own
physics (the noise figure, the interference figures) belongs to the ether,
not to the ground.

## Nodesets

A nodeset is which nodes stand where, what they run, and how they are set:

```yaml
# testbed/nodesets/mitte7.yaml
nodes:
  internet: { id: 1, lat: 52.5219, lon: 13.4132, height_m: 38, height_from: assumed,
              antenna: { gain_dbi: 2 }, device: stable, role: transport, tags: [] }
  gw02:     { id: 2, lat: 52.5208, lon: 13.4094, height_m: 30, height_from: assumed,
              antenna: { gain_dbi: 2 }, device: stable, role: transport,
              radio: { freq_mhz: 869.525, sf: 8, bw_khz: 125, tx_dbm: 14 },
              tags: [lora, tcp-peer, lxmf] }
offsets:                              # dB added to one pair's computed loss, both ways
  - { between: [internet, gw02], db: 40, note: "wall, measured 2026-09-20" }
```

A node is its position in degrees, its antenna's height above the ground and
where that figure came from (`measured`, `roof`, `raster`, `assumed`), its
antenna gain, its tags, and three **declared settings**:

- `device`: what it runs ([Devices](#devices));
- `role`: `transport` or `client`; absent, the firmware's own default;
- `radio`: slot 0's `freq_mhz`, `sf`, `bw_khz`, `cr`, `tx_dbm`, and
  optionally `sync` and `preamble`; absent, the radio is left as the
  firmware starts it.

The page and the medium read the declared settings without running
anything: the double ring of a transport node, the coverage, and the bands
a simulation computes tables for come from them. At setup they are said to
the station in its kind's own lines ([Scripts](#scripts)).

**A node's name is how everything refers to it**: scripts, offsets,
snapshots, the map, and the proxy's hostnames. **Its id is its network
identity**: it fixes the station's loopback address in the simulation's
network (node 1 is `127.16.0.5` in the front's first network) and the MAC
(media access control) address the station derives, so two nodes never
share one. A new node takes the lowest free id. Changing an id moves the
station: anything it was set up with from its old `{id}` or `{addr}` is
stale, so a station that has state is restarted, and the page says so.

**Offsets** are dB added to one pair's computed loss, both ways, on any
geodata: where a measurement says the model is wrong, and by how much, with a
note of where the figure came from. They are a layer over the loss table,
applied when the medium is given it, so the table stays the model's own and
an offset never forces a recompute.

A nodeset is offered on every geodata whose extent holds one of its nodes.
`testbed/nodesets/gen_town100.py` writes the `town100` nodeset and its
scripts from a seed: 100 stations in a 6 by 4 km town on `plain-35` at SF7
(spreading factor 7), checked against the formula's neighbour counts and hop
diameter.

## Scripts

A script is Python against SIMesh's own library, `simesh`:

```python
# testbed/scripts/smoke7.py (abridged)
async def setup(node):                      # once per station with no state
    await node.run("auth passwd admin admin")
    if "tcp-peer" in node.tags:
        await node.peer_tcp("internet", 4965)
    if "lxmf" in node.tags or node.name == "internet":
        await node.run("lxmf create {name}")

async def main(sim):                        # a driver, on a running simulation
    await sim.all_up()
    await sim.nodes(tag="lora").announce(spread=60)
```

It has either or both of two entry points, and may have a report.

**`setup(node)`** runs inside the simulation's simd, once per station, when a
station boots with no state: a node just placed, every station of a new
simulation, a station after a factory reset. A station is set up in this
order: its declared name, role and radio figures in its kind's lines, then
the script's `setup`, then whatever starts its radio (`lora up`), so that a
setting the radio reads when it starts is in place by then. Then it is
flushed. `node` has the station's `name`, `id`, `tags`, `device`, `kind`
(its kind's type), `role` and `radio`; `node.run(line)` types a line in its
own language with the macros filled in, and `set_name()`, `set_role(role)`,
`set_radio(**figures)`, `announce()` and `peer_tcp(other, port)` say an
intent; `node.addr(other)` is another node's address. `setup` runs in simd's
own loop, so it awaits and never blocks. A simulation started with a script
keeps a copy of it in its run, and a snapshot keeps that copy.

Lines keep these macros until a station is given them:

| Macro | Becomes |
|---|---|
| `{name}` | the node's name — `alpha` |
| `{id}` | its station id — `1` |
| `{addr}` | its loopback address in the simulation's network — `127.16.0.5` |
| `{addr:<node>}` | another node's loopback address, by name — `tcp peer add {addr:internet}:4965` |

Anything else in braces is left exactly as written. The declared settings and
the script are the whole of what a station is told: nothing is added behind
your back. They do not run again on an ordinary reset, because the station
is already set up, so `lxmf create {name}`, which run twice makes two
identities, is safe in `setup`.

**`main(sim)`** is a driver. The front runs the script as a process of its
own (`python3 -m simesh.runner`), attached to a running simulation over the
front's socket, with its output kept and shown on the Scripts tab; from a
shell,

```sh
simesh run lxmf-traffic --sim lora
simesh run ./my-study.py --geodata plain-35 --nodeset town100 --time max
```

attaches to a running one, or asks the front for a new one set up by the
script's own `setup`, when it has one and is one of `scripts/` (a simulation
keeps its setup script by name; a file from elsewhere only drives it); the
Scripts tab's Run can name another setup script instead. A simulation the
run started is **paused** when `main` ends: its stations stop with their
state kept in its run, and it stays on the Simulations tab to be resumed
([Pausing](#pausing-and-resuming)). `sim` is a `simesh.Sim`:

| | |
|---|---|
| `sim.nodes(tag=, device=, kind=, role=, names=)`, `sim.node(name)` | a selection of stations, in id order |
| `sel.run(line, kind=)` | a line on each, in its own language; a selection of more than one kind needs `kind` |
| `sel.set_name()`, `set_role(role)`, `set_radio(**figures)`, `announce()`, `message(to, text)`, `path(to)`, `peer_tcp(to, port)`, `address()` | an intent on each, in its kind's lines; a kind that cannot say one answers `! …` for that station |
| `sel.reset()`, `sel.factory_reset()` | pressed on each |
| `spread=`, `after=` on any of them | spread the stations over that many seconds, and hold the whole back that many, of the run's clock |
| `sim.run_s`, `sim.until(t)`, `sim.sleep(s)`, `sim.all_up()` | the run's clock, in seconds since attaching |
| `sim.plan((name, until), …)` | the phases, for the page's estimate |
| `sim.snapshot(name)`, `sim.move(name, lat, lon)`, `sim.pause()`, `sim.stop()` | the simulation itself |
| `sim.pairs(sample=, seed=)`, `sim.rng` | ordered pairs of stations, and a random source |
| `sim.stations`, `sim.run_dir` | what is known of each station, and where the run's files are |

Each answer is `{station: reply}`. `simesh.start(geodata, nodeset, script=,
time=)` and `simesh.attach(name)` give a `Sim` outside the runner. Every time
is the run's, so a real-time and a virtual-time run act at the same instants
of the run.

**`report(run_dir)`**, `def` or `async def`, runs after `main` has returned
(and after the pause), and returns the run's report as Markdown. The runner
writes it to the run as `report.md`, and the page's **Report** buttons, on
the script run and on the simulation's row, show it. `simesh run SCRIPT
--report RUN_DIR` writes it again for a run that has ended.

`scripts/lxmf-traffic.py` is a whole LXMF (Lightweight Extensible Message
Format) run: announce warm-up until paths stop growing, a seeded hour of
sends, a drain, a gather, with its record written to the run as
`traffic.json`. Its own `setup` gives each station what taking part needs,
a device password (without one Reticulum is held, and with it the LoRa
interface) and an LXMF identity; started with another setup script, that one
must. Its report is the delivery `delivery.py` counts: overall, by route and
radio hops and by size, the latency, and the undelivered by the sender's
last line; it says so plainly when Reticulum was held for want of a
password, or when no station had an LXMF identity to send from.

## Loss tables

A loss table is every ordered pair's path loss for one nodeset on one
geodata, in one band (433, 868 or 915 MHz), computed at one frequency in the
band; the ether adds `20·log10(f/f0)` per frame to move it to the frame's own
carrier. It is derived, never edited, and cached under
`testbed/losses/<geodata>/<nodeset geometry>/<band>.bin`, keyed by what it
depends on: the geodata's content and the nodeset's node set, positions and
heights. Names, ids, gains, devices, radios, tags and offsets leave it alone.
[LOSSTABLE.md](LOSSTABLE.md) is the file format. A simulation computes a
table for each band its nodes' declared carriers fall in, 868 when none
declares one.

- **Synthetic ground's table** is computed in-process, log-distance, in
  moments.
- **A pack's table** is computed through the sidecar's `/link.json`, one
  request per ordered pair: P.1812 where the path allows, a near-field
  model where the two are too close for it, with the model used and whether
  the first Fresnel zone is clear kept per cell. The planner judges at
  869.525 MHz, 50 % of time and 90 % of locations, so a pack has an 868
  table only. The sidecar answers one request at a time, a few milliseconds
  each: 169 nodes is a few minutes.
- **Every pair is computed**, not only pairs strong enough to carry a frame,
  because a pair far too weak to decode still adds to a receiver's
  interference. Pairs more than 30 km apart, or with an end off the pack,
  are never heard.

The front computes a simulation's tables before its stations start, with
progress on the page, and copies them into the run. A node moved during a
run has its row and column recomputed into the run's copy, never into the
cache; until it lands the ether keeps the old row. From a shell:

```sh
python3 testbed/losses.py --geodata berlin-city --nodeset mitte7 --band 868 --sidecar http://127.0.0.1:<port>
```

prints progress as JSON lines and the cached table's path.

## Who can hear whom

There are no stated links. Where a node stands, on its geodata, is the whole
of it: the level a frame arrives at is

```
L = P_tx + G_tx + G_rx − loss(tx → rx) − offset − 20·log10(f / f0)
```

with `P_tx` the power the frame went out at, the gains the nodes' antennas,
the loss the table's and the offset the nodeset's. A frame that arrives below
the signal-to-noise ratio its spreading factor needs — −7.5 dB at SF7, down
to −20 dB at SF12 — is not delivered at all, and that is what "out of range"
means here. So a link is in range only if it is one the modem could actually
hold, and moving to a slower spreading factor really does reach further.

Every transmission whose channel overlaps a receiver's is interference
there, whatever its spreading factor, and each receiver rules for itself: a
frame survives where it clears thermal noise by its spreading factor's
threshold and each spreading factor's summed interference by that class's
rejection figure, over every stretch of its air.
[`ether/README.md`](ether/README.md) has the whole of what the medium
decides, and [INTERNALS.md](INTERNALS.md#reception-at-the-receiver) why.

## The page

Its tabs are **Devices**, **Geodata**, **Nodes**, **Scripts** and
**Simulations**, and a status line under them all says how many simulations
and scripts are running (a simulation's name there puts the Nodes tab on
it). Served by a simd on its own, the page is that one simulation's Nodes
tab.

**Devices** lists every device: what it is called, what it plays, its kind,
its catalogue (or local build), when it was built, and which catalogue name
resolves to it, three per catalogue at most. **Import…** takes a device zip;
**Refresh** fetches the newest from the web and from `builds/` beside SIMesh
([Devices](#devices)).

**Geodata** lists the geodata with its kind and extent. Clicking a row shows
that geodata on its own, with no nodes, scrollable and zoomable, and makes it
the one the Nodes tab lists nodesets for; **‹ Back** returns to the list.
**Import…** takes a pack zip, and **New synthetic…** makes flat synthetic
ground at an exponent and an extent.

**Scripts** lists the scripts with their first docstring line, edits one
(saved through the front, which checks it parses), and **Run…** starts its
`main` on a running simulation or on a new one (set up by the script chosen
there, or by its own), with its output beside the editor as it comes, and
**Stop** while it runs. A new one is paused when `main` ends.

**Simulations** is the registry ([Running simulations](#running-simulations))
and **Run from current world**: every simulation, running, paused, or ended
(every run directory under `testbed/runs/` that is neither, by its name).
Clicking a running simulation's row opens it: its live map, in place of the
list, with **‹ Simulations** (or the tab itself) back to the list; it stays
open while other tabs are on show, and going to the Nodes tab closes it. Its **⋯** menu
pauses it, resets or factory-resets all its stations, saves a snapshot of
it, or loads one into it; **Stop** ends it. A paused one has **Resume**; a
paused or ended one has a trash can, which deletes its run directory. A row
whose run has a report, as its script's `report` wrote it, has **Report**.
Each row shows its simulated time T and the real time it has been running
(for a stopped one, from its start to its end; T from its pause, or from the
last line of its record), and how fast T runs; a real-time run's T is its
real time, so it shows only that. An open simulation's toolbar shows
the same for the simulation it is attached to. Both read as a clock,
`01:33:24`, and from a day on as `2d + 03:12`.

### The Nodes tab

Standalone, it starts as a list of the nodesets with a node on the geodata
clicked last on the Geodata tab, and a **Start empty** line; clicking a row
opens it on the map. **Import CSV…** there makes one from the planner's
`sites.csv` or the deployed-network CSV (comma-separated values) `planner
nodes import` writes (a height where the file has none, marked assumed; a
transmit power column into each node's radio). An open nodeset is the page's
own until **Save**, which writes it back to its own file (amber while there
is something to save; it keeps the file's leading comment); one started empty
is asked a name at its first Save. **‹ Nodesets** goes back to the list,
asking first when there are unsaved edits. The Nodes tab edits nodesets and
nothing else. The same map, opened from a running simulation's row on the
Simulations tab, is that run's live map: the same edits go to the run's own
copy of its nodeset (never to `nodesets/`), and **Save nodes as nodeset…**
keeps them.

**The map** is one canvas, drawn in the geodata's own metres. From the
ground up:

- **ground**: for a pack, the planner's base map shaded by terrain or by
  clutter height, composed by planner-wasm from the sidecar's `tile.bin` and its
  server-side bake; on synthetic ground, a grid in metres or degrees at 1, 2
  or 5 times a power of ten, the brighter cross at 0°, 0°, and the extent's
  square;
- **roads and railways** from the sidecar's `roads.bin`, drawn as lines;
- **buildings** from `buildings.bin`: outlines under 9 km across (wider,
  the map says to zoom in to see them), filled by height above the ground
  under 4 km (slate low, sand about 20 m, red 60 m and up); a pack built
  without `--lod2-geometry` has none. They are fetched in 1 km squares
  (smaller where one reply would be cut short by the sidecar's vertex cap),
  nearest the middle first, and kept, so every part of the view fills in
  and panning back costs nothing;
- **population**, a pack's residents per cell from its population layer, as
  a heatmap from a faint violet trace to a dense orange-yellow;
- **coverage**: the best margin over the decoding threshold at each point
  from any node on show, at each node's declared power, gain, spreading
  factor and bandwidth, in bands of what it is good for: **green** 21 dB and
  more (reception indoors too: 15 dB of walls on top of the edge's 6),
  **yellow** 6 to 21 dB (outdoors only), **red** 0 to 6 dB (the edge, where
  fading decides), nothing below 0 (no chance);
- **offsets** as dashed lines with their dB;
- **links**, for the one selected node: every other node it reaches, coloured
  by the level it would be heard at, green to amber where it decodes, red
  where it only interferes, dashed where the first Fresnel zone is not clear;
  from the run's table, or standalone from the saved nodeset's, computed
  (or taken from the cache) when a node is selected with this layer on, its
  progress at the top of the map, and again after a save that moved a node;
- **the nodes**: a dot each with its name, its antenna's height above the
  ground (`20 m up`) and, smaller, its tags; a second, black ring round a
  node whose role carries others' traffic;
- **live**, attached: a ring from a transmitter for as long as the frame
  occupies the air, a flash at each receiver (green clean, red CRC (cyclic
  redundancy check) failure), the dot's colour its status (grey stopped,
  amber starting or in setup, white up, red restarting), and a dashed amber
  ring round a moved station whose row has not landed.

**Selecting.** A click picks a node; Shift-click toggles one in or out.
Ctrl or Cmd and a drag draws a rectangle that picks what is in it, with Shift
added to the selection, with Alt taken out of it. A plain drag on a node moves
it, and every other selected node with it; any other drag pans, and the
wheel zooms about the cursor. The **tags panel** lists every tag with how many
nodes carry it, and adds the nodes carrying one to the selection, or takes
them out.

**The editor** is on the selection, one node or many. Every field shows the
value the nodes share, or `<multiple values>` where they differ, and a field
changed there changes on every selected node and no other field with it:
device, position (one node), height, gain, and tags. A node's Reticulum
role is not set here: the double ring on the map says a node carries
others' traffic, and attached, the editor shows the role the station
reports. The nodeset file's `role` is what it declares, and a setup script
can set it (`node.set_role(…)`). The **height** is the antenna's, in metres above
sea level, with its height above the ground under it beneath the box; to
its right, in small print, the **terrain** there and, when the node stands
inside a building's footprint, that building's **roof**, both above sea
level, and a click on either puts the antenna there. (The nodeset keeps the
height above the ground; on synthetic ground the terrain is 0 m.) The trash can in
its header removes the selected nodes, after asking. A node's radio is not edited here: a setup
script sets it (`node.set_radio(…)`), and the nodeset file's `radio` is what
it declares. A tag on all of
them is filled and on some is outlined with how many; clicking it puts it on
every one, its × takes it off every one, and **add a tag** puts a new one on
all, the other tags untouched. For one node it also lists the **extra
losses** (offsets) it has, each opening its pair and removable with its ×.
**Clicking a link line** opens the pair's inspector: the planner's own
`link.json` for the two, with the antennas' heights (and an indoor end's
entry loss), the terrain profile, the table's cell beside it, and the pair's
**extra loss**, set there with a note of why. Attached,
one node's editor shows its status, live role and radio and who hears it at
what level, with **Console**, **Web UI**, **Reset** and **Factory reset**.

**Right-click** on a node: attached, **Console**, **Web UI**, **Reset**,
**Factory reset**, **Announce** and **Run command…**; and **Remove this
node…** (or these). On the ground: **New node here**, **New node on this roof**,
select all or none, and fit the view to the nodes. A new node takes the
lowest free id, `stable`, and the default radio (869.525 MHz, SF8, 125 kHz,
14 dBm).

**Coverage** is a heatmap, on by default: the selected nodes' coverage while
any are selected, the whole network's while none are, and the key at the
map's foot says whose. It is computed only while the tab is on show and the
heatmap is coverage. On synthetic
ground it is the log-distance formula, worked out on the page. On a pack each
node's raster is the planner's point-to-area sweep to a receiver 2 m above
the ground within 10 km, which the front has the sidecar compute one node at a
time and caches by the node's position and height (`testbed/coverage/`), so
changing power or radio redraws at once and moving a node sweeps only that
one; rasters in the cache draw at once and the rest as they land.

**The display menu** (☰) says how the map is shown: what the ground is
shaded by and its roads and buildings, or the grid's units; the heatmap,
none, population or (on the Nodes tab) coverage, one at a time, with its
key at the map's foot, while everything under it but the nodes and the
selected node's links is drawn grey; and on the Nodes tab the links and offsets layers and whether nodes
show their names, heights and tags. The Geodata tab has the same menu without
the node entries. Each tab
keeps its own choices, per browser, and the view is remembered per geodata.

**Attached**, the toolbar shows the phase, the time mode and T, and how many
stations are up; what is done to stations is on the right-click menu of the
selection (Console, Web UI and Reset for one; Factory reset, Announce and Run
command… for all selected), and what is done to the whole simulation is on
its row of the Simulations tab. Every edit is logged in the run with its T.

## Running simulations

**Run from current world** on the Simulations tab starts the Nodes tab's
nodeset on its geodata in real time, set up by the script chosen beside it or
by the declared settings alone, for working with the stations by hand. A
simulation runs the nodeset's file, so whenever one is started from the
nodeset the Nodes tab is editing (here, or from a script's Run), its unsaved
changes are saved first. A new node's device is `stable` when there is a
stable device on this machine, else the first catalogue that has one.
`simesh new` from a shell picks geodata, a nodeset and a script, or a
snapshot, then a time mode and a build. The front checks
them, resolves every node's device (so a missing one is said at once),
computes or reuses the loss tables, lays out the run directory and starts a
simd on it.

**The time** is how the run keeps it, for the ether and every station alike:

| | |
|---|---|
| `real` | the wall clock; a person is in the loop at the pace of a real network |
| `max` | virtual time as fast as the stations allow |
| `<k>x` (paced) | virtual time paced at k times the wall clock: `10x` is ten minutes of the network per wall minute, `0.5x` half speed |

In virtual time the ether owns the run's clock, T, and moves it only when
every station has nothing left to do at the T it has, so what a network does
in an hour takes the stations' own work to run and not an hour, and a loaded
host slows a run down instead of changing it. A frame occupies T for its
time on the air, a station's timers and sleeps run on T, the record and
`seq.py` are stamped with T, and simd logs the T at which a run loaded and
each station came up. How is in [INTERNALS.md](INTERNALS.md#time) and the
wire in [`ether/README.md`](ether/README.md#virtual-time). The Nodes tab,
attached, shows the mode, the pace (for `max`, the pace lately reached) and
T, and a transmission's ring lasts its time on the air at the run's pace.

In a virtual-time run what reaches a station from outside the air lands at
an instant of T, and T waits while it is dealt with: a staggered start, a
line or framed RPC query typed at a console (and the next one, typed when
the reply to it is read), a TCP (Transmission Control Protocol) write from
one station to another, and work a station spends more than a tick of the
host's time on. So with the same `--seed` and `--epoch`, two runs of the same
geodata, nodeset and script put the same frames on the air at the same T. A
station's TCP CLI or web UI reached from outside the run, and whatever a
person does, still arrives at whatever T the run has reached; a driver that
means to act at an instant of the run asks simd to wait on T first (`after`,
[below](#the-control-websocket)).

**Stations start spread over a minute**, not all at once. Two dozen firmware
processes forking in the same instant is a thundering herd against one host,
and the network it makes is worse than the load: stations that boot together
announce together, so the opening minute is a collision storm no network
powered up by hand would ever have. `--stagger <seconds>` changes the
spread; `0` starts them together. The map fills in as they come up.

### The front, and the simulation verbs

```
browser ── localhost:8800 ─────────────────────► front.py ─┬─► simd (lora)  127.0.0.1:9100
browser ── alpha.lora.sim.localhost:8800 ──────►           ├─► simd (supe)  127.0.0.1:9101
driver  ── localhost:8800/ws?sim=lora ─────────►           └─► …
```

`simesh` runs the front, `testbed/front.py`, on the published port 8800. It
starts one simd per simulation behind it, each with its own loopback control
port (from 9100), ether port (from 7100), station network (a /22 from
`127.16.0.0` that no socket on the host is bound in and no other front holds
the lock of, so two fronts on one host never share one) and run directory
(`testbed/runs/<name>/`, with the simd's own log in `simd.log`). Arguments
after `--` go to every simd as they stand: `simesh -- --pairwise --stagger
30`.

**Simulations** lists each one: its nodeset, geodata and script, its pace
and T, the phase its driver says it is in with a bar of the plan, when it
should be done, and how many of its stations are up; a simulation that exited
says so with its last lines, and a paused one where T stood when it paused.
Clicking a running one's row opens its live map.

### Pausing and resuming

```
runner ── sim_pause {name} ──► front     main ended, on a simulation started for it
front: stop its simd (stations flushed), runs/<run>/paused/ ← the run as a snapshot
front ── sims {…, {name, state: paused, t}} ──► every page
page ── sim_resume {name} ──► front: runs/<run>/paused/ → runs/<name>-N/, a simd on it
```

A **paused** simulation is stopped with everything a snapshot keeps (its
nodeset, script, tables and every station's state) kept inside its own run
directory, as `paused/`, instead of among the snapshots. It stays in the
registry, across a restart of the front too, until it is resumed or deleted.
**Resume** loads that into a new run directory under the same name, as a
snapshot load would, and starts it: the same network as it ended, booting
again, with T from 0 on a new ether; the paused run is then listed as ended.
Its trash can deletes the run directory, and with it the pause. A script's run pauses the simulation it started when
`main` ends; **⋯ ▸ Pause**, `simesh pause`, and `sim.pause()` pause any running
one.

From a shell, `simesh`'s simulation verbs do the same (`simesh help` lists
every verb); `new` starts the front in the background (logging to
`testbed/runs/front.log`) when nothing answers on the port. Under Docker
they run inside the front's container, `simesh-front`, so the front is
started first with `simesh`.

```sh
simesh new lora --geodata plain-35 --nodeset town100 --script town100-lora --time max
                                                  # prints its control address, ether, network and run as JSON
simesh new pw --geodata plain-35 --nodeset town100 --script town100-lora --time max --pairwise
                                                  # the same, its ether on the pairwise rule
simesh new --snapshot town100-warm --time 2x --build dev
                                                  # named after what it loads: town100-warm
simesh list
simesh plan lora warm-up=+600 traffic=+4200       # T each phase ends at; +N is N s from now
simesh pause lora                                 # stopped, its state kept; listed as paused
simesh resume lora                                # started again as it ended, in a new run
simesh stop lora
simesh run lxmf-traffic --sim lora                # a script's main: one of scripts/ by name, or a path
```

A driver never picks a port or a network for a simulation: it asks the front
for one and drives it through `ws://127.0.0.1:8800/ws?sim=<name>`.

### One simd by hand

On Linux or in SIMesh's image, one simulation is one process:

```sh
cd SIMesh/testbed && python3 simd.py
```

That starts the ether, the stations, the proxy and the control page, on
`http://localhost:9011/`, and loads the run its run directory holds, if it
holds one; otherwise it waits for a driver to send `sim_load`. It runs in the
foreground: Ctrl-C stops it, and everything it started. It takes `--bind`
(default `0.0.0.0:9011`), `--ether` (default `127.0.0.1:7000`), `--run` (the
run directory, default `testbed/runs/simd/`; a run loaded later that would
land on one there goes beside it as `-2`, `-3`…), `--build`, `--sidecar` (the
planner-web a pack's moved rows are recomputed through), `--stagger`,
`--net`, `--time`, `--noise-figure` and `--pairwise` (the ether's receivers
and its rule), and `--seed` and `--epoch` (the seed the ether's welcome
carries, which in a virtual-time run also keys every station's randomness,
and the wall clock T 0 stands for; two runs of one network given both draw the
same random bytes and the same timestamps).

A simd started beside the front, or beside another, needs its own port,
ether, station addresses and run directory, because every station binds its
own address, two stations on one address are one port taken twice, and two
testbeds in one run directory would write over each other:

```sh
python3 simd.py --bind 0.0.0.0:9012 --ether 127.0.0.1:7001 --net 127.0.4.0/22 --run /tmp/run2
```

takes its addresses from `127.0.4.0/22` instead of `127.0.0.0/22`. A network
is filled one /24 at a time with hosts 5 to 254: node 1 is the network's
first `.5`, node 250 its `.254`, node 251 the next /24's `.5`. The default
/22 holds 1000 stations; a wider network holds more. The front's children
take their networks from `127.16.0.0/22` up and their ports from 9100 and
7100 up, stepping around any port already taken, so a network below
`127.16.0.0` is one no child will be given.

## Runs and snapshots

A **run** is one simulation's output, `testbed/runs/<name>/`, and the
directory its stations run in:

| Path | What it is |
|---|---|
| `run.yaml` | the geodata, nodeset and script names, the time mode, each device's build by stamp, when it started, and the snapshot it started from |
| `geodata.yaml`, `nodeset.yaml` | the geodata as it was, and the run's own copy of the nodeset: edits during the run go here, never to `nodesets/` |
| `script.py` | the script it was started with, as it was, when it had one |
| `losses/<band>.bin` | the loss tables the ether reads, the run's own copies |
| `nodeset-edits.jsonl` | every nodeset edit during the run, with its T |
| `nodes/<name>/state/` | the station's state store |
| `nodes/<name>/log` | everything the station wrote to its console, across restarts |
| `record.tsv` | every message in and out of the ether |
| `simd.log` | the simd's own log |

A **snapshot** is a moment of a run, taken with **⋯ ▸ Save snapshot as…** on
the simulation's row (or `snapshot_save_as` on the control websocket, or
`sim.snapshot`),
`testbed/snapshots/<name>/`:

| Path | What it is |
|---|---|
| `snapshot.yaml` | taken at which T, from which run, which builds |
| `geodata.yaml`, `nodeset.yaml` | the geodata and the nodeset, as the run had them |
| `script.py` | the run's script, when it had one |
| `losses/<band>.bin` | the run's tables |
| `nodes/<name>/state/` | every station's store |

A simulation given a snapshot (**⋯ ▸ Load snapshot into it…** on its row, or
started from one with `simesh new --snapshot`) gets its nodeset, script and tables
back exactly as they were, without a recompute and without the planner, and
every station its store: identities, keys, paths and message history, as the
firmware keeps them. The snapshot keeps its script because setup runs only on
a station with no state: a factory reset after the load sets a station up as
the first run did.

What comes back is what the firmware reloads at boot. A `reticulous` station
keeps its identities, keys and message history, but its paths only when
`s.rnsd.dir.persist_routes` is `1` (the default `0` drops restored routes and
relearns them on demand), and only as of its last directory write, every
`s.rnsd.dir.persist_s` seconds (default 900): a snapshot is a copy of the
store, and the store does not hold the path table in between.

Stations keep running across a snapshot: they are flushed first and the copy
is taken while they run, which for a store that commits whole files is the
same guarantee a power cut gives a board. Logs and the record are an account
of one run and are never copied into a snapshot. Runs, snapshots, the loss
and coverage caches and the fetched devices are not committed.

## The verbs

**On one station** (the editor, or the right-click menu) **and on the whole
simulation** (the ⋯ menu on its Simulations row):

| | |
|---|---|
| **Reset** | presses reset. The process exits and comes straight back; its state is untouched, so it is the same station it was. |
| **Factory reset** | wipes its state, restarts it, and it is set up again on the empty store. Identities, keys, paths and message history go; the nodeset and the script do not. |

Across the whole simulation both are spread over the same `--stagger`
window the start uses, and for the same reason.

**Right-click ▸ Run command…** on the Nodes tab types one line at the
selected stations, and lists what each one said. A
line is one kind's language, so the stations must be of one kind; the dialog
asks which when the simulation has more than one. The macros are expanded per
station, so `lora 0 freq 869.475` retunes every `reticulous` station and
`announce now` at the `berlinmesh` kind makes every one of those announce.
**Right-click ▸ Announce** is the `announce` intent, in each station's own
lines.

Beside the line is **spread**, in seconds. Left at 0 every station is asked at
once, which is what a question wants — nothing goes on the air to answer
`show s.net.hostname`. Anything that *transmits* wants a spread: two dozen
stations running `lora 0 a` in the same instant is a collision storm rather
than an announcement, and what comes back describes the storm. Thirty or sixty
seconds across the network is enough.

### The control websocket

```
page/driver → simd   command {line, name? | names? | tag?, kind?, stagger?, after?, id?}
page/driver → simd   meta {verb, args?, name? | names? | tag?, kind?, stagger?, after?, id?}
simd → all pages     command_result {id, line | verb, name, results: {<station>: <reply>}, t}
driver → simd        plan {phases: [{name, until}]}
simd → all pages     clock {mode, rate, t, observed, barriers, slow_idles, plan}   once a wall second
```

Everything the page does is one JSON message on `ws://<simd>/ws`, and a
driver speaks the same messages; `testbed/simd.py`'s docstring lists them all
(`sim_load`, `snapshot_load`, `snapshot_save_as`, the `nodeset_*` edits by
node name, `levels`, the resets). `command` goes to the stations that are up
(or in setup) among those named by `name`, `names` or `tag`, or to all; they
must be of one kind or `kind` must narrow them. `meta` is an intent (the
[kinds' table](#station-kinds), and `address`), said to each chosen station
in its own lines; `message` and `path` take `to`, a node whose address is
asked of it first, and `peer_tcp` takes `to` and `port`. `stagger` spreads
the stations over that many seconds; `after` holds the whole back that many
seconds on the run's clock, so in a virtual-time run it lands at an instant
of T. Its answer is one `command_result`, broadcast to every page: the
asker's `id`, each station's reply by name (a failure as `! why`), and `t`,
the run's clock when the last reply came. `clock` says how the run keeps
time — `rate` the pace asked for (null: as fast as it goes), `observed` the
pace of the last wall second, `t` the run's clock in microseconds,
`barriers` how often T has moved and `slow_idles` how many idles have arrived
at the busy watchdog's pace ([INTERNALS.md](INTERNALS.md#time)); the
`snapshot` a page gets on connecting carries one too.

`plan` is a driver saying what the run is for: its phases in order, each
with the T it ends at in microseconds. simd keeps it until the next `plan` or
the next load, and puts it in every `clock` as `{t, phases}`, `t` being when
it was given; `phases: []` clears it. From it the page shows the phase, how
far into it the run is, and when it will be done at the pace of the last two
minutes. The LXMF traffic driver sends one once every station is up and again
whenever its warm-up runs on.

`ws://<simd>/ws?quiet=1` is a socket without `tx`, `rx`, `radio` and `levels`
— everything only a map draws, and nearly all the traffic on a busy network.
A driver wants it.

Through the front the same messages pass both ways, plus the front's own:

```
page/driver → front   sims                                       the registry, as it stands
page/driver → front   sim_new {name?, geodata, nodeset, script? | snapshot, time?, stagger?, build?, pairwise?}
front → asker         sim_new {ok, name, control, ether, net, run, time, geodata, nodeset, script, snapshot, builds}
                              or {ok: false, error}
front → every socket  losses_progress {sim, band, done, total}  while its tables are computed
page/driver → front   sim_stop {name}                            stop it, or forget one that exited
front → asker         sim_stop {ok, name} or {ok: false, error}
page → front          select {sim}                               the simulation this socket is on
front → all sockets   hello {front: true, port}                  on connecting
front → all sockets   sims {port, sims: [...], script_runs: [...], geodata_names, nodesets, scripts, snapshots}
                                                                 on a change and once a wall second
front → all sockets   script_output {run, line} · script_exit {run, code}
anything else         → the simulation named by `sim`, or the one selected
simd → socket         the simulation's own message, with `sim` added
```

`ws://<front>/ws?sim=<name>` selects a simulation from the start and
`&quiet=1` asks for its quiet stream. A row of `sims` carries the
simulation's `state` (`starting`, `running`, `stopping`, `exited` with its
`code` and last lines in `tail`), the `geodata`, `nodeset`, `script` or
`snapshot` it loaded, `time`, `mode`, `rate`, `t`, `pace` (T per wall second
over the last two minutes), station `counts` by status, its `plan`, the
current `phase` `{name, from, until, eta}` and `eta`, the wall time the last
phase should end, and where it is: `port`, `ether`, `net`, `run`. `GET
/api/sims` is the same as JSON.

The front's editor verbs are answered to the asking socket as `{type, ok,
…}`, and need no simulation running:

| Verbs | |
|---|---|
| `device_list`, `device_refresh {sources?}` | the devices, and a fetch of the catalogues |
| `geodata_list`, `geodata_open`, `geodata_close`, `geodata_new`, `geodata_save`, `geodata_save_as` | geodata; opening a pack holds its sidecar for the socket |
| `nodeset_list {geodata?}`, `nodeset_open`, `nodeset_new`, `nodeset_save`, `nodeset_save_as`, `nodeset_import` | nodesets; with `geodata`, each row says whether a node stands on it |
| `script_list`, `script_open`, `script_new`, `script_save`, `script_save_as` | scripts, checked to parse |
| `script_run {name, sim \| geodata, nodeset, time?}`, `script_stop {run}`, `script_log {run}` | a script's `main` as a process, and its output |
| `snapshot_list`, `losses_compute` | the snapshots, and a nodeset's tables |
| `coverage {geodata, nodes}` | each node's pack raster: cached ones at once, the rest as `coverage_tile` messages as they land |

and its HTTP side: `POST /api/devices/import?name=<zip name>` and `POST
/api/geodata/import?name=<geodata>` take a zip as the body; `GET
/api/table?path=` serves a loss table and `GET /api/coverage?geodata=&key=` a
coverage raster; `/planner/<geodata>/…` is the pack's sidecar. `front.py`'s
docstring has every field.

## Reading a run

Every analysis tool reads a run directory, and takes names, positions, radio
settings and levels from it — the run's nodeset (its declared radios and
roles, a node declaring no role a `client`), its geodata, the devices it
resolved and its own loss tables with the offsets on them — never the files
as they stand now. What a frame means is a protocol's, under
`testbed/simesh/<protocol>/`, found by each node's device kind; the roles are
what `airtime.py --roles` and the hop counts through forwarding stations use.

| Tool | Says |
|---|---|
| `seq.py RUN` | the record as a sequence diagram (below) |
| `compare.py RUN_A RUN_B` | two runs side by side: milestones, per-station counts, paths; one run's figures alone |
| `airtime.py RUN` | airtime per station, per carrier and per frame kind; transmit power; exchanges at reduced power; CRC losses; with `--busy` the calling channel's occupancy where each station stands, with `--roles` airtime per role |
| `links.py RUN` | link geometry: distance of every usable one-way link, neighbours, hop diameter, beside what the run's loss table says would decode; with `--power`, traffic-channel power against distance |
| `delivery.py TRAFFIC.json RUN` | an LXMF traffic run's delivery (the `traffic.json` `scripts/lxmf-traffic.py` writes) from the senders' logs: by route hops, radio hops (by the run's loss table), size, latency |

`seq.py` draws a run's `record.tsv` — one lifeline per station, one arrow
per station that heard a frame, the verdict at each arrow head, and the
Reticulum packet read out on the right:

```
$ python3 seq.py runs/lora --tail 4
   t (s)   delta    india    kilo     mike     papa    sierra
   0.118     │        ◀────────┼────────┼────────┤        │  ANNOUNCE  single/4e3874cc  of rnstransport.probe  hops=0  167B
             │        │        │        │        ├────────▶
   0.250     ✗────────┼────────┼────────┼────────┼────────┤  ANNOUNCE  single/edec275b  of rnstransport.probe  hops=0  167B
             │        │        │        │        ✗────────┤
```

One transmission heard by two stations is two arrows on two rows, sharing the
timestamp and the reading: every arrow has one end at the station that
transmitted, so nothing in the picture can be read as a frame travelling
between two stations that cannot hear each other. `--only <words>` keeps the
rows whose reading matches, and `--record <file>` reads a record on its own
(its lifelines are then ids, or `--names 1=alpha,2=bravo`). In a run with no
Reticulous station, a frame reads as its length and carrier.

## A station's own doors

A `reticulous` station serves its web UI on port 80 of its own loopback
address, which is invisible outside the machine or image SIMesh runs in; the
proxy on port 8800 routes by hostname, the simulation being the second
label (a station of a kind with no web UI is refused with a sentence saying
so, and has no **Web UI** button):

    http://alpha.lora.sim.localhost:8800/    by name
    http://1.lora.sim.localhost:8800/        by id

A bare `alpha.sim.localhost` reaches the one running simulation while there
is exactly one; with more it is refused with their names. Chrome and Firefox
resolve any `.localhost` name to loopback with no configuration. Safari does
not, and needs entries in `/etc/hosts` on the Mac:

    127.0.0.1  alpha.lora.sim.localhost bravo.lora.sim.localhost

Port 8800 is the testbed's own: where `simesh` runs SIMesh in its image, the
container publishes it at the same number on the host. It is clear of the
ports spangap holds (9000–9011), the planner's 8787, and the per-simulation
control and ether ports the front counts up from 9100 and 7100, so a testbed,
a flashmon and a dev server can all run at once.

A station's web UI is the **whole** UI, not a static shell: it speaks the same
WebRTC DataChannel to the browser that a board does, from the same firmware
source, so the live panes — settings, the log, the CLI, Activity — all work.

```
browser ──ws  alpha.lora.sim.localhost:8800/webrtc──► front ──ws──► simd ──ws──► alpha   signalling
browser ──udp localhost:8800─────────────────────────► front ──udp─► simd ──udp─► alpha   the channel
```

The DataChannel is UDP and the station's own address is out of the
browser's reach, so the signalling passes through the front and the simd,
each of which points the station's SDP (session description) answer at
itself, and each relays the UDP behind it, picking the station out of each
packet by the ICE (interactive connectivity establishment) ufrag it saw in
that answer. Port 8800 is published on **UDP as well as TCP** for it
([INTERNALS.md](INTERNALS.md#the-webrtc-relay-and-why-it-is-a-relay-rather-than-a-second-transport)).
Neither end knows. The station is answering ICE from a peer that happens to
be a relay, and the browser is talking to a station that happens to be
simulated — which is the point: the code under test is the shipping code,
on both sides.

Besides the map, a station is reachable three other ways:

- **Console** — its serial console, in a terminal window over a websocket.
  For `reticulous`, first-run setup and every CLI command, exactly as a board
  on a cable; for `berlinmesh`, its log lines.
- Its kind's own door. simd sets up and asks a `reticulous` station over
  **framed RPC** on its console pty (below), and a `berlinmesh` one with
  `rncfg <verb> runs/<sim>/nodes/<name>/kiss`, which talks KISS (the serial
  framing radio modems speak) to it exactly as over USB. A `reticulous`
  station's TCP CLI is closed, as on a board, until `set s.net.cli_port 8081`
  opens it (a script line does that for a whole nodeset); then `nc <addr>
  8081` from a shell on the same machine is its command line.
- `tail -f runs/<sim>/nodes/<name>/log` — everything it has printed, across
  restarts.

### Framed RPC on the console

```
station → simd   "… serial] framed rpc v1"                 once, early in boot, as log text
simd → station   F5 53 47 01 <id> <len:2> show s.net.hostname
station → simd   F5 53 47 01 <id> <len:2> s.net.hostname = alpha
simd → station   F5 53 47 01 <id'> <len:2> show s.sys.reset_reason   until it reads: boot is done
simd → station   F5 53 47 01 <id''> <len:2> lora up          one frame per setup line
station → simd   F5 53 47 01 <id''> <len:2> enabled 1 radio(s)
```

The firmware multiplexes a framed side channel onto its serial console
([`spangap-core/docs/framed-rpc.md`](../spangap-core/docs/framed-rpc.md)), and
a station's console here is its pty. A frame is never echoed, never enters the
line editor and never turns the log into a CLI session, so simd asks a station
things while a person types at its **Console**. The pty drain takes each reply
frame out of the stream and passes every other byte on unchanged, so the log
and the console window never see one.

A station answers its door once it has printed the marker since it last
started and answered `show s.net.hostname` with something. The command line
answers from early in boot, before the firmware's services have initialised
their settings, and a setting typed then can be undone by that init (a
radio's frequency is); the boot writes `s.sys.reset_reason` once they all
have, and a first boot, which is when a station is set up, has none until
then. So a station is `up` once that reads and it has been set up, if it had
to be. A station that prints no marker within 20 seconds of wall time is sent
the probe once, blind, and a Ctrl-C after it if it does not answer; that
undoes it on firmware that does not speak frames, and the station stays
`starting`.

The device runs one frame at a time, bounds each at five seconds, and cuts a
reply that outgrows its buffer at the last complete line without saying so.
So simd keeps one frame in flight per station and asks for one key at a time,
and a setup line whose effect lands after its reply is followed until it has:
`lora up` by `show s.lora.0.enable` until it reads `1`, and any line whose
reply came back at the five-second bound by `show s.net.hostname` until the
command line answers again. The id is a hash of the command, so a retry
carries the one it had, and a reply that arrives after its query gave up
answers the retry.

A command line of up to 4096 bytes runs over a frame, so an `lxmf send`
carrying a text that needs a link and a resource goes the same way as any
other line; a longer one is refused with `rpc: command over 4096 bytes`.

A station that exits is started again, because a restart on this target is a
process exit: a station rebooting itself comes back on the same address with
the same directory.

## The developer loop, with a workspace

To run firmware of your own, put SIMesh in a [spangap](https://github.com/spangap/spangap)
workspace — the directory it was cloned into, made one with `spangap init` —
and build the station there, for the `hw-linux` board, a Linux process
rather than a chip image:

```sh
spangap build reticulous/reticulous --with spangap/hw-linux \
    --with reticulous/netgraph \
    -x reticulous/rnsh -x reticulous/iface-auto -x reticulous/iface-ble \
    -x reticulous/rnode-ble -x reticulous/nomad -x reticulous/maps \
    -x spangap/viewer -x spangap/acme -x spangap/duckdns -x spangap/sshd \
    -x spangap/upnp -x spangap/wg
```

The excluded straddles are the ones not built for this target; netgraph is
there because a nodeset whose stations share a community
(`s.netgraph.community`) needs it, the community's membership announce being
what carries each node's gateway distance. The result is
`reticulous/esp-idf/build.linux/`: `reticulous.elf` and its `/fixed` tree in
`data_merged/`. Every target builds in its own `build.<target>/`, so a chip
build and this one never touch each other's files.

The local device `reticulous-workspace` is that directory: name it as a
node's `device:`, or as the build of one simulation (the page's build field,
`simesh new … --build reticulous-workspace`), and rebuild, then start a
new simulation or factory-reset the stations, whenever the firmware changes.
The build is run in place, so a station restarted after a rebuild runs the
new binary.

A `berlinmesh` station and its tool are built in the Sergeyculum tree
(`sergey/reticulum` in the workspace), with Rust, and are the local device
`berlinmesh-workspace`:

```sh
cd sergey/reticulum/fw/simesh && cargo build --release    # fw/simesh/target/release/simesh
cd sergey/reticulum && cargo build --release -p rncfg     # target/release/rncfg
```

Device files are produced by the catalogue build: a `builds.yaml` entry with
`target: linux`, `arch:` and `stands_for:` becomes `hw-simesh-<arch>` in its
catalogue when `spangap make-builds` runs on a machine of that architecture.
The front copies it in from the workspace's `builds/<catalogue>/` by itself
([Devices](#devices)).

## After a restart

Stations are processes, not a service: stopping `simesh` stops them. Their
state is not in the process though — `runs/`, `snapshots/`, `devices/` and a
workspace's build are all in the directory SIMesh was cloned into, which the
image mounts from the host. A simulation's stations keep their state in its
run directory, but a new simulation starts factory-fresh; to carry a network
across a restart, save a snapshot before stopping, then after `simesh` load
it into a new simulation (⋯ ▸ Load snapshot into it…), and it comes back with its names, radio
settings, identities and message history as they were.

## The pieces on their own

The medium is its own program with its own docs:
[`ether/README.md`](ether/README.md) for what it does and the wire it
speaks, [`ether/INTERNALS.md`](ether/INTERNALS.md) for how. It runs alone
against hand-written stations, taking its nodes from a nodeset and its
losses from a directory of tables:

```sh
python3 ether/ether.py --bind 127.0.0.1:7000 --record record.tsv \
        --geodata testbed/geodata/<geodata>.yaml --nodeset testbed/nodesets/<nodeset>.yaml \
        --losses <dir holding 868.bin>
```

The proxy also runs on its own, for a station set started some other way; alone
it routes by station id only, since names are the nodeset's:

```sh
python3 testbed/proxy.py --bind 0.0.0.0:8800
```

## The chip library

`radio/` is the SX1262 model and the station's link to the ether, behind a
C ABI (application binary interface, `radio/include/simradio.h`): open the
link, open a chip per radio slot, hand it SPI frames, pulse its reset, read
its lines, and, in a virtual-time run, read node time, set wakes, learn
every move of T and say the station is idle. A station of any language
links it in place of a radio, below an unchanged driver. Beside it,
`radio/shim/simclock.c` builds `libsimclock.so`, the preloaded library that
answers the C library's clocks and waits in node time, draws the station's
randomness from the run's seed, counts the console and TCP bytes the ether
makes into instants of T, and keeps the busy watchdog off a station that is
computing ([INTERNALS.md](INTERNALS.md#time)).

The model reaches its host through six services — a clock, one-shot timers, a
recursive lock, a UDP socket, a reader, a log — and two backends supply them:

| Backend | For |
|---|---|
| `radio/backend/posix/` | a plain process: `std::thread`, `CLOCK_MONOTONIC`, a `std::recursive_mutex` |
| `radio/backend/esp-idf/` | an ESP-IDF (Espressif's development framework) firmware built for the Linux host target: esp_timer, a FreeRTOS critical section and task; an IDF component |

`simesh build radio` builds it (`radio/build/`: `libsimradio.a`,
`libsimradio.so`, `libsimclock.so`); a virtual run preloads the shim from
there. The ESP-IDF backend is proved by a throwaway project that links it
against the IDF host port and sends one frame (`radio/tests/esp-idf-link/`;
the commands are at the top of its `CMakeLists.txt`).

## Tests

None needs firmware, a planner or a network:

```sh
cd SIMesh/testbed && python3 -m pytest -q      # the stores, the devices, the front, simd, the kinds, the library, the tools
cd SIMesh/ether   && python3 -m pytest -q      # the medium, over real UDP and in-process
cd SIMesh/radio   && python3 -m pytest -q tests  # the chip model, the conductor, the time shim
cd SIMesh/testbed/ui && npx vue-tsc --noEmit && npx quasar build
```

The model's tests load `libsimradio.so` with ctypes, drive it frame by frame
the way a driver does, and play the ether on a UDP socket of their own; the
conductor's tests do the same in virtual time, and the shim's run a small C
stand-in station (`radio/tests/standin.c`) under `libsimclock.so`. The
testbed's pack tests run against a real `planner-web` when it is built in
`planner/` and the `berlin-city` pack is in `packs/`, and are skipped otherwise.

## Where the code lives

Two halves. The **host port** is what makes a firmware build and run as a
process at all; the **simulation** is what gives it a radio and a medium.

### The host port of reticulous

It lives with that firmware, not here:

| Where | What |
|---|---|
| [`spangap/build-system`](../spangap/build-system/README.md) | a board straddle's `target:`, exported as `IDF_TARGET`; on `linux`, no flashable image, and a device file in a catalogue |
| [`spangap/hw-linux`](../hw-linux/README.md) | the board: station identity and directory, the GPIO (general-purpose input/output) shim, esp_timer, descriptor waits, the tickless tick |
| `spangap-core/esp-idf/src/host/` | no power manager, no USB transport, and deflate over the system zlib |
| `spangap-net/esp-idf/src/net_relay.cpp` | the socket relay, shared with the chip: the event bus, the listen sockets, the byte proxy |
| `spangap-net/esp-idf/src/host/` | the link backend — loopback, up from the first instant — in place of the WiFi state machine |
| `spangap-web/esp-idf/src/host/` | the WebRTC port: the addresses to advertise, the address to bind, a CRC32; the rest of the DataChannel is the chip's source |

Everywhere else the rule is the same: chip-only code sits behind
`#if !CONFIG_IDF_TARGET_LINUX` or drops out of the source list, and host-only
code lives in that component's `src/host/`.

### The simulation

| Where | What |
|---|---|
| `simesh` | the one command: the front natively or in SIMesh's image, `new`, `stop`, `list`, `plan`, `run`, `devices`, `build` |
| `Dockerfile` | SIMesh's image, for a machine that is not Linux |
| `devices/local/` | the local devices: builds of one's own, named |
| `radio/` | the chip and the station's UDP link to the ether, as a C library |
| `radio/src/conductor.cpp` | the station's side of virtual time: T, node time, wakes, the idle |
| `radio/shim/simclock.c` | `libsimclock.so`, the C library's time in node time, the seeded randomness, the console and TCP counts and the watchdog's hold; `radio/include/simclock.h` is what it is handed |
| `iface-lora/esp-idf/src/host/virtual_hal.*` | RadioLib's HAL (hardware abstraction layer) over the GPIO shim and `radio/`, in place of the SPI bus |
| [`ether/`](ether/README.md) | the medium: the loss tables, who hears a frame and how it comes out; `slt.py` reads and writes a table |
| `testbed/front.py` | several simulations behind one port: the registry, the editors' verbs, the imports, the planner sidecars, the loss tables before a start, one simd per simulation, script runs, coverage, station hostnames by simulation, the WebRTC relay one level up, the finish estimate |
| `testbed/simd.py` | one simulation: the ether, the stations and their setup, the proxy, the control server, commands and intents on chosen stations, a moved node's row |
| `testbed/simctl.py` | behind `simesh new`, `stop`, `list` and `plan`: the front from a shell; starts the front when none answers |
| `testbed/store.py` | where geodata, nodesets, scripts, tables, coverage, runs and snapshots live, and what a name may be |
| `testbed/devices.py` | devices: fetch, import, expand, local devices, and what a `device:` names |
| `testbed/geodata.py` | geodata: packs and synthetic ground, the projections, the extent, a pack's import |
| `testbed/nodeset.py` | nodesets: nodes and their declared settings, offsets, edits, the geometry hash, the CSV imports |
| `testbed/script.py` | scripts: listing, checking, loading |
| `testbed/losses.py` | a loss table, on synthetic ground or through the sidecar; the cache; offsets as a layer; one node's row |
| `testbed/coverage.py` | a node's coverage raster on a pack, through the sidecar, cached |
| `testbed/runs.py` | a run directory, and snapshots taken from and loaded into one |
| `testbed/stations.py` | one firmware process, its pty, its log, its supervisor; the thread every station's pty is read on |
| `testbed/kinds/` | one class per firmware: its environment, when it is up, how it is set up and asked things, its role, its intents |
| `testbed/rpc.py` | framed RPC on a station's console pty: the demultiplexer in the drain, and the client its kind speaks |
| `testbed/proxy.py` | the hostname proxy |
| `testbed/webrtc.py` | the WebRTC relay: the signalling rewritten, and one UDP port in front of every station's DataChannel |
| `testbed/ui/` | the page (Quasar 2 on Vue 3; Pinia stores `catalog`, `geodata`, `nodes`, `sim`, `coverage`, `display`, `socket`); `vendor/planner-wasm` is the planner's built planner-wasm, copied in by `vendor/update-planner-wasm.mjs` so the page builds with no planner beside it |
| `testbed/seq.py`, `compare.py`, `airtime.py`, `links.py`, `delivery.py` | the analysis tools ([Reading a run](#reading-a-run)) |
| `testbed/simesh/` | the library: `sim` (a driver's hold on a simulation, selections, intents), `setup` (one station as a script's `setup` sees it), `runner` (a script's `main` run on a simulation), `view` (a run opened for analysis), `record`; `simesh/reticulum/` holds Reticulum's parts: frame reading (Reticulum packets, SUPE), the LXMF traffic driver, delivery analysis |
| `testbed/scripts/` | the scripts: setups for the nodesets here, and `lxmf-traffic.py` |
| `testbed/nodesets/gen_town100.py` | writes the `town100` nodeset and the scripts `town100-lora`, `town100-supe` (SUPE, channel plan 1) and `town100-supe0` (SUPE, channel plan 0) from a seed |

Nothing above the bus is aware of any of it: the LoRa driver, its CSMA and
airtime accounting, Reticulum, LXMF and the web UI are the same code that runs
on a board. The same holds for a `berlinmesh` station: its SX1262 driver,
`LoRaIface` and engine are Sergeyculum's own, unchanged, over an embedded-hal
bus that ends in `radio/`.
