import { defineStore } from 'pinia'
import { useSocket } from './socket'
import { useCatalog, type GeodataInfo } from './catalog'
import { readTable, type LossTable } from '../lib/slt'
import type { HeightFrom, NodesetData, Radio } from './nodes'

/** The run a simulation is, as simd's snapshot describes it (runs.Run.as_dict). */
export interface RunInfo {
  name: string
  dir: string
  geodata?: string
  nodeset?: string
  script?: string | null
  snapshot?: string | null
  [key: string]: unknown
}

/* The live half: the running simulations, the one the Nodes tab is attached
 * to, and everything its simd streams. Every action a person takes on a
 * running simulation is one method here that sends one message, which the
 * front passes to that simulation's simd. Reconnecting replays simd's
 * snapshot, so nothing here is state simd cannot restate.
 *
 * Served by the front, messages from a child carry its `sim`, and those from
 * any other simulation than the attached one are dropped. Served by a simd on
 * its own there is one simulation, no registry, and no `sim` on anything. */

export type Status = 'stopped' | 'starting' | 'setup' | 'up' | 'restarting'

/** One station, as simd's `node` message describes it. */
export interface Node {
  name: string
  id: number
  /** Its kind's type: reticulous, berlinmesh, … */
  kind: string | null
  /** The device reference its node names, and what the device is called. */
  device: string
  device_name: string | null
  /** Whether it has a web UI the proxy can reach. */
  web: boolean
  lat: number
  lon: number
  height_m: number
  height_from: HeightFrom
  gain_dbi: number
  tags: string[]
  declared_role: string | null
  declared_radio: Radio | null
  /** What it does for the mesh, as its kind reads it live: transport,
   *  router, repeater, client, or null when the kind cannot say. */
  role: string | null
  status: Status
  /** Moved, and its row of the loss table not yet recomputed: the ether is
   *  still using the old one. */
  stale: boolean
  mode?: string
  freq?: number
  sf?: number
  bw?: number
}

/** A transmission in the air, on the browser's clock: the ether's microseconds
 *  are its own and mean nothing here, but a frame's duration does. */
export interface Pulse {
  eid: number
  name: string
  start: number              // performance.now() when the tx arrived
  duration: number           // ms the frame occupies the air
}

/** One reception, drawn as a flash at the receiver in the verdict's colour. */
export interface Flash {
  name: string
  from: string
  verdict: 'clean' | 'crc'
  level: number
  start: number
}

/** How the run keeps time: real, or virtual at a pace (`rate` seconds of T
 *  per second of wall, null for as fast as it goes) that simd has lately
 *  `observed`. */
export interface Clock {
  mode: 'real' | 'virtual'
  rate: number | null
  observed: number | null
  t: number
  /** What the driver said the run is for, when one did. */
  plan?: { t: number; phases: PlanPhase[] } | null
}

/** One phase of a driver's plan: its name and the T it ends at, in µs. */
export interface PlanPhase { name: string; until: number }

export interface LossProgress {
  band: string
  done: number
  total: number
  running: boolean
  cached?: boolean
  error?: string | null
}

/** One simulation in the front's registry. Times are the wall's, in seconds
 *  since the epoch; T is the run's, in microseconds. */
export interface SimSummary {
  name: string
  /** `ended`: a run on disk, neither running nor paused, named by its directory. */
  state: 'starting' | 'running' | 'stopping' | 'exited' | 'paused' | 'ended'
  code: number | null
  /** When an ended one was started, ISO. */
  started_at?: string | null
  /** Whether its run has a report.md. */
  report?: boolean
  /** When it was paused, ISO, for a paused one. */
  paused_at?: string | null
  geodata?: string | null
  nodeset?: string | null
  script?: string | null
  snapshot?: string | null
  dirty: boolean
  /** The `--time` it was started with: real, max or <k>x. */
  time: string
  mode: 'real' | 'virtual' | null
  rate: number | null
  observed: number | null
  /** Seconds of T per second of wall over the last two minutes. */
  pace: number | null
  t: number | null
  stations: number
  counts: Partial<Record<Status, number>>
  plan: PlanPhase[] | null
  /** The T the plan was given at: where its first phase begins. */
  plan_from?: number
  phase: { index: number; name: string; from: number; until: number; eta: number | null } | null
  /** When the last phase should end, by the pace. */
  eta: number | null
  done?: boolean
  /** Wall seconds it started at, and stopped at (null while it runs). */
  started: number | null
  ended?: number | null
  port: number
  ether: string
  net: string
  run: string
  errors: [number, string][]
  /** The last lines it wrote, once it has exited. */
  tail: string[]
  [key: string]: unknown
}

/** What a new simulation is made from. */
export interface SimSpec {
  name?: string
  geodata?: string
  nodeset?: string
  script?: string
  snapshot?: string
  time: string
  build?: string
}

export type Tab = 'devices' | 'geodata' | 'nodes' | 'scripts' | 'sims'

const FLASH_MS = 400
const MAX_PULSES = 400       // a busy network, bounded

export const useSim = defineStore('sim', {
  state: () => ({
    /** The attached simulation's run, geodata, nodeset and script, from simd's snapshot. */
    run: null as RunInfo | null,
    /** The run's loss table for the band on show, for the links layer. */
    table: null as LossTable | null,
    geodata: null as GeodataInfo | null,
    nodeset: null as NodesetData | null,
    script: null as { name: string; setup: boolean; main: boolean } | null,
    bands: [] as string[],
    /** The last command or intent and what each station said back. */
    command: null as { what: string; results: Record<string, string> } | null,
    nodes: {} as Record<string, Node>,
    pulses: [] as Pulse[],
    flashes: [] as Flash[],
    /** What each node is heard at by the others, from a `levels` request. */
    levels: {} as Record<string, Record<string, number>>,
    /** The station web UIs' port: the front's, or a lone simd's own. */
    port: '8800',
    clock: { mode: 'real', rate: 1, observed: null, t: 0 } as Clock,
    errors: [] as string[],
    notices: [] as string[],
    sims: [] as SimSummary[],
    /** The simulation the Nodes tab is attached to, or null. */
    selected: null as string | null,
    /** Which tab is on show. */
    view: 'nodes' as Tab,
    /** The front's answer to this page's last `sim_new`. */
    lastNew: null as { ok: boolean; name?: string; error?: string } | null,
    /** Loss tables being computed for a simulation, by its name: before its
     *  child starts, and for a load or a move once it runs. */
    progress: {} as Record<string, LossProgress>,
    /** The medium's settings, from the snapshot. */
    medium: { noise_figure_db: 6, pairwise: false } as { noise_figure_db: number; pairwise: boolean },
  }),

  getters: {
    nodeList: (s): Node[] => Object.values(s.nodes).sort((a, b) => a.id - b.id),
    loaded: (s): boolean => s.nodeset !== null,
    attached: (s): boolean => s.selected !== null,
    dirty: (s): boolean => s.nodeset?.dirty ?? false,
    running: (s): number =>
      Object.values(s.nodes).filter(n => n.status === 'up').length,
    /** Whether any station is a Reticulous one, which is what shows the
     *  page's Reticulum verbs. */
    reticulum: (s): boolean => Object.values(s.nodes).some(n => n.kind === 'reticulous'),
    /** The run's station kinds. */
    kinds: (s): string[] => [...new Set(Object.values(s.nodes).map(n => n.kind ?? '?'))].sort(),
    /** Seconds of the run's time per second of the browser's: 1 in real
     *  time, the pace of a paced run, or what an unpaced one lately did. */
    speed: (s): number => {
      if (s.clock.mode !== 'virtual') return 1
      const r = s.clock.rate ?? s.clock.observed
      return r && r > 0 ? r : 1
    },
    /** The attached simulation's row in the registry. */
    current: (s): SimSummary | null => s.sims.find(x => x.name === s.selected) ?? null,
    /** How many simulations are running, for the status line. */
    runningSims: (s): number => s.sims.filter(x => x.state === 'running').length,
  },

  actions: {
    reconnected() {
      // A front forgets a socket's selection with the socket.
      if (this.selected) this.send({ type: 'select', sim: this.selected })
    },

    receive(msg: Record<string, unknown>) {
      const front = useSocket().front
      if (msg.type === 'losses_progress' && typeof msg.sim === 'string') {
        const done = msg.done as number, total = msg.total as number
        this.progress[msg.sim] = { band: msg.band as string, done, total, running: done < total }
      }
      if (!front || msg.sim === undefined) {
        // The front's own messages, or a lone simd's.
        switch (msg.type) {
          case 'sims': {
            this.sims = (msg.sims as SimSummary[]) ?? []
            for (const name of Object.keys(this.progress)) {
              const row = this.sims.find(s => s.name === name)
              if (row && !this.progress[name]!.running) delete this.progress[name]
            }
            // An ended run may share the name; only a live simulation keeps the tab on it.
            if (this.selected && !this.sims.some(s => s.name === this.selected
                                                  && s.state !== 'ended' && s.state !== 'paused')) this.detach()
            return
          }
          case 'sim_new':
          case 'sim_resume':
            this.lastNew = msg as { ok: boolean; name?: string; error?: string }
            if (msg.ok) this.attach(msg.name as string)
            else this.errors.push(msg.error as string)
            return
          case 'sim_stop':
          case 'sim_pause':
          case 'run_delete':
            if (!msg.ok) this.errors.push(msg.error as string)
            return
          case 'error':
            if (front) { this.errors.push(msg.text as string); return }
            break
          case 'notice':
            if (front) { this.notices.push(msg.text as string); return }
            break
        }
        if (front) return
      }
      // A child's message from before this page attached to another simulation.
      if (front && msg.sim !== this.selected) return
      this.child(msg)
    },

    /** One message of the attached simulation's simd. */
    child(msg: Record<string, unknown>) {
      switch (msg.type) {
        case 'snapshot': {
          this.run = (msg.run as RunInfo) ?? null
          this.geodata = (msg.geodata as GeodataInfo) ?? null
          this.nodeset = (msg.nodeset as NodesetData) ?? null
          this.script = (msg.script as typeof this.script) ?? null
          this.bands = (msg.bands as string[]) ?? []
          if (msg.medium) this.medium = msg.medium as typeof this.medium
          if (!useSocket().front) {
            // Served by a simd on its own, the page is its one simulation.
            this.port = String(msg.port ?? this.port)
            this.selected = this.selected ?? ''
            this.view = 'nodes'
            useCatalog().names(msg)
          }
          if (msg.clock) this.clock = msg.clock as Clock
          this.nodes = {}
          for (const node of (msg.nodes as Node[]) ?? []) this.nodes[node.name] = node
          this.pulses = []
          this.flashes = []
          this.levels = {}
          void this.loadTable()
          break
        }
        case 'node': {
          const node = msg as unknown as Node
          const was = this.nodes[node.name]
          // Keep the radio fields a `radio` message put here: a status change
          // says nothing about the carrier and must not blank it.
          const { type: _t, sim: _s, ...fields } = msg
          this.nodes[node.name] = { ...was, ...fields } as Node
          // A moved node's row has landed in the run's table.
          if (was?.stale && !node.stale) void this.loadTable()
          break
        }
        case 'node_gone':
          delete this.nodes[msg.name as string]
          break
        case 'nodeset':
          this.nodeset = msg as unknown as NodesetData
          break
        case 'store':
          useCatalog().names(msg)
          break
        case 'losses_progress':
          if (!useSocket().front) {
            const done = msg.done as number, total = msg.total as number
            this.progress[''] = { band: msg.band as string, done, total, running: done < total }
          }
          break
        case 'command_result':
          this.command = {
            what: (msg.line ?? msg.verb) as string,
            results: msg.results as Record<string, string>,
          }
          break
        case 'radio': {
          const node = this.nodes[msg.name as string]
          if (node) Object.assign(node, {
            mode: msg.mode, freq: msg.freq, sf: msg.sf, bw: msg.bw,
          })
          break
        }
        case 'clock':
          this.clock = msg as unknown as Clock
          break
        case 'tx': {
          // The frame's time on the air, in the browser's milliseconds: a
          // virtual-time run that goes ten times the wall draws it a tenth
          // as long, so a ring still lasts exactly the frame's span.
          const span = (msg.t_end as number) - (msg.t_start as number)
          this.pulses.push({
            eid: msg.eid as number,
            name: msg.name as string,
            start: performance.now(),
            duration: Math.max(60, span / 1000 / this.speed),
          })
          if (this.pulses.length > MAX_PULSES) this.pulses.splice(0, this.pulses.length - MAX_PULSES)
          break
        }
        case 'rx':
          this.flashes.push({
            name: msg.name as string,
            from: msg.from as string,
            verdict: msg.verdict as 'clean' | 'crc',
            level: msg.level as number,
            start: performance.now(),
          })
          break
        case 'levels':
          this.levels[msg.name as string] = msg.heard as Record<string, number>
          break
        case 'notice':
          this.notices.push(msg.text as string)
          break
        case 'error':
          this.errors.push(msg.text as string)
          break
      }
    },

    /** Drop everything that has finished animating. Called from the map's frame. */
    expire(now: number) {
      if (this.pulses.length) this.pulses = this.pulses.filter(p => now - p.start < p.duration)
      if (this.flashes.length) this.flashes = this.flashes.filter(f => now - f.start < FLASH_MS)
    },

    send(msg: Record<string, unknown>) {
      const socket = useSocket()
      socket.send(socket.front && this.selected ? { sim: this.selected, ...msg } : msg)
    },

    /** The run's own copy of its first band's loss table, for the links layer. */
    async loadTable() {
      const band = this.bands[0]
      const dir = this.run?.dir
      if (!band || !dir) { this.table = null; return }
      const run = this.run
      try {
        const r = await fetch(`/api/table?path=${encodeURIComponent(`${dir}/losses/${band}.bin`)}`)
        if (r.ok && this.run === run) this.table = readTable(await r.arrayBuffer())
      } catch { /* the links layer falls back to the levels alone */ }
    },

    /** Forget the simulation on show, for the next one's snapshot to fill. */
    clear() {
      this.run = null
      this.table = null
      this.geodata = null
      this.nodeset = null
      this.script = null
      this.nodes = {}
      this.pulses = []
      this.flashes = []
      this.levels = {}
      this.command = null
      this.clock = { mode: 'real', rate: 1, observed: null, t: 0 }
    },

    /* ── simulations, through the front ── */
    /** Open a running simulation's live map: on the Simulations tab, as its
     *  row's detail (a simd on its own has only the one tab). */
    attach(name: string) {
      if (name !== this.selected) {
        this.clear()
        this.selected = name
        useSocket().send({ type: 'select', sim: name })
      }
      this.view = useSocket().front ? 'sims' : 'nodes'
    },
    /** Close it, back to the list; the simulation keeps running. */
    detach() {
      if (this.selected === null) return
      this.clear()
      this.selected = null
      useSocket().send({ type: 'select', sim: null })
    },
    /** A tab. The Nodes tab edits nodesets and nothing else, so going to it
     *  closes a simulation's live map; the other tabs leave it open, there
     *  on the Simulations tab to come back to. */
    show(view: Tab) {
      if (view === 'nodes' && useSocket().front) this.detach()
      this.view = view
    },
    newSim(spec: SimSpec) {
      this.lastNew = null
      useSocket().send({ type: 'sim_new', ...spec })
    },
    stopSim(name: string) { useSocket().send({ type: 'sim_stop', name }) },
    /** Stopped with its state kept, to be resumed as it ended. */
    pauseSim(name: string) { useSocket().send({ type: 'sim_pause', name }) },
    resumeSim(name: string) { useSocket().send({ type: 'sim_resume', name }) },
    /** An ended or paused run deleted, directory and all; `run` is its directory's name. */
    deleteRun(run: string) { useSocket().send({ type: 'run_delete', run }) },

    /* ── the attached simulation's nodeset, by node name ── */
    addNode(name: string, lat: number, lon: number, fields: Record<string, unknown> = {}) {
      this.send({ type: 'nodeset_add', name, lat, lon, ...fields })
    },
    /** A drag tells simd a few times a second (`settle` false) and once more
     *  where it ends; only the settled move is written and recomputed. */
    moveNode(name: string, lat: number, lon: number, settle = true) {
      const node = this.nodes[name]
      if (node) { node.lat = lat; node.lon = lon; if (settle) node.stale = true }
      this.send({ type: 'nodeset_move', name, lat, lon, settle })
    },
    setNode(name: string, fields: Record<string, unknown>) {
      this.send({ type: 'nodeset_set', name, ...fields })
    },
    removeNode(name: string) { this.send({ type: 'nodeset_remove', name }) },
    setOffset(a: string, b: string, db: number, note?: string) {
      this.send({ type: 'nodeset_offset', between: [a, b], db, ...(note ? { note } : {}) })
    },
    /** Press reset: the process goes and comes back, state untouched. */
    resetNode(name: string) { this.send({ type: 'node_reset', name }) },
    /** Wipe its state and start it again, set up afresh. */
    factoryResetNode(name: string) { this.send({ type: 'node_factory_reset', name }) },
    askLevels(name: string) { this.send({ type: 'levels', name }) },

    /* ── the run ── */
    startAll() { this.send({ type: 'start_all' }) },
    stopAll() { this.send({ type: 'stop_all' }) },
    resetAll() { this.send({ type: 'reset_all' }) },
    factoryResetAll() { this.send({ type: 'factory_reset_all' }) },
    /** One line on the stations named (every one when none), `{name}` and
     *  friends expanded. `stagger` spreads the stations over that many
     *  seconds — 0 fires them together, which is wrong for anything that
     *  transmits. */
    runCommand(line: string, stagger = 0, kind: string | null = null, names: string[] | null = null) {
      this.command = null
      this.send({ type: 'command', line, stagger, ...(kind ? { kind } : {}), ...(names ? { names } : {}) })
    },
    /** An intent on the stations named, each in its own kind's lines. */
    runIntent(verb: string, args: Record<string, unknown> = {}, stagger = 0, names: string[] | null = null) {
      this.command = null
      this.send({ type: 'meta', verb, args, stagger, ...(names ? { names } : {}) })
    },
    saveSnapshotAs(name: string) { this.send({ type: 'snapshot_save_as', name }) },

    /** The station's own web UI, through the proxy on this same port; behind
     *  the front the simulation is the second label. */
    stationUrl(name: string) {
      const socket = useSocket()
      const label = socket.front && this.selected ? `${name}.${this.selected}` : name
      return `${location.protocol}//${label}.sim.localhost:${socket.front ? socket.port : this.port}/`
    },

    /** The station's console websocket. */
    consoleUrl(name: string) {
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
      const path = useSocket().front && this.selected
        ? `/ws/console/${this.selected}/${name}` : `/ws/console/${name}`
      return `${proto}//${location.host}${path}`
    },
  },
})
