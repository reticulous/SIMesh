# The station contract

What SIMesh gives every station process, and what it expects back. A firmware
that keeps this contract, and links `radio/` below its radio driver, runs on
the testbed; what differs between firmwares beyond it is a **kind**
(`testbed/kinds/`).

## The environment

| Variable | Meaning |
|---|---|
| `SIMESH_NODE_ID` | a small integer, unique on the host; the last byte of any MAC the station makes, and the `sid` it gives the ether |
| `SIMESH_NODE_DIR` | the station's directory; its working directory; its state lives under `state/` |
| `SIMESH_BIND_ADDR` | its own loopback address, fixed by its id in the testbed's network (`simd --net`, a /22 holding 1000 stations by default); every socket it opens binds here |
| `SIMESH_ETHER` | `host:port` of the ether |
| `SIMESH_TIME` | `virtual` in a virtual-time run, absent in a real-time one |
| `SIMESH_EPOCH_US` | virtual time: the wall-clock microseconds T 0 stands for |
| `LD_PRELOAD` | virtual time: `radio/build/libsimclock.so`, the time shim |
| `SIMESH_IDLE` | virtual time, set by a kind whose firmware does not call `simradio_idle()` itself: `threads`, and the shim says the station is idle when every thread is blocked (below) |
| `SIMESH_CLOCK_PROFILE` | optional, from a kind's `env:`: node time as a function of T, `T:node,T:node,…` in microseconds, both columns increasing, slope 1 outside the points. Absent, node time is T |
| the kind's `env:` | anything the binary needs beyond that |

Nothing else is promised. A station reads its identity from these and from
nowhere else, so two stations on one host never collide.

## The process

- **stdin and stdout are the console**: a pty, text, shown to a person in the
  map's console window and appended to `log` in its directory. The one binary
  thing that may cross it is a framed-RPC frame
  ([`spangap-core/docs/framed-rpc.md`](../spangap-core/docs/framed-rpc.md)),
  which the supervisor takes out of the stream before anything else sees it;
  a kind that speaks it says so, and the rest print text only.
- **Exit to reboot.** The supervisor starts the binary again, on the same
  directory and address, half a second later (of T, in a virtual-time run). A
  station that wants to reboot exits.
- **State is its directory.** Loading a scenario empties `state/`; a factory
  reset empties it; a reset leaves it. Whatever marks "this directory has been
  set up" is the kind's to name.
- **Ports are its own**, on its own address. Which ones, and what answers on
  them, is the kind's to say.

## The radio

The station links `radio/` (`simradio.h`) and calls
`simradio_station_open(SIMESH_NODE_ID, SIMESH_BIND_ADDR, SIMESH_ETHER)` once,
then `simradio_open(slot, …)` per radio. Its driver talks to the chip model
frame by frame, exactly as to an SX1262 on a bus.

The medium matches receivers on carrier, bandwidth, spreading factor and sync
word, and **does not model preamble length**: two radios whose preambles
differ hear each other here and may not on a bench. Set them equal in a
scenario that means to say anything about hardware.

## Time

In a real-time run a station keeps the host's time. In a virtual-time run
([INTERNALS.md](INTERNALS.md#time)) the ether owns time, and a station keeps
three promises:

- **It reads time only through the C library or `radio/`.** The shim answers
  `clock_gettime`, `gettimeofday`, `time`, the sleeps, `setitimer` and the
  timeouts of `poll`, `select`, `epoll_wait` and `pthread_cond_timedwait` in
  node time; the model's own timers are on T. A raw `rdtsc`, a `clock_gettime`
  made by system call, or a wait that none of those is does not move with the
  run.
- **It opens its link early**, before anything in it waits on time:
  `simradio_station_open` is where the station's clock starts and the shim
  attaches, and until the ether's welcome the monotonic clocks read 0 and the
  wall clocks the run's epoch.
- **It says when it is idle, and until when.** Idle is every thread
  blocked; the `until` it reports is the earliest wake anything in it holds
  (`simradio_wake_at`), which is the instant it next needs to run, and
  nothing sooner. Either the host calls `simradio_idle()` itself — a FreeRTOS
  station from its tickless idle, with a wake at the tick its first task is
  due at and another at esp_timer's next expiry, so a station whose tasks
  sleep for a second is woken once in that second — or the kind sets
  `SIMESH_IDLE=threads` and the shim keeps a census of the process's threads
  and says so for it, each sleeping thread's deadline a wake. A station that
  says neither is reported idle by the library's watchdog, 20 ms of wall
  time after every message, which runs but crawls. A host whose own clock is
  counted from node time, as a kernel tick is, learns of every move of it
  from `simradio_on_advance()`.

A kind waits between two questions to a station with `pause()`, which is on
T in a virtual run, so a poll costs the station the same time in either mode.

## A kind

A scenario names its kinds under `kinds:`; a kind is a class in
`testbed/kinds/` that says, for one firmware:

| | |
|---|---|
| `env` | the environment above, plus what that firmware reads |
| `wait_up` | when a started station counts as up |
| `pause` | a wait on the run's clock, for a kind's polls |
| `run` | how one setup line, or one **Run command** line, is put to it |
| `flush` | how to make what it was told durable, before a stop or a snapshot |
| `transport` | whether it forwards for others, or unknown |
| `web_port` | the port of its web UI, or none |
| `configured` | whether its directory has been set up already |
