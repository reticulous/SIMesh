import { defineStore } from 'pinia'
import { request } from '../lib/front'
import type { AntennaSpec } from '../lib/antennas'
import { useSocket } from './socket'

/* What the store holds, for every picker and list on the page: devices,
 * antennas, geodata, nodesets, scripts, snapshots, and the script runs the front keeps
 * with their output. The registry the front sends every second names what
 * there is; the *_list verbs describe it, and a name alone keeps what a
 * description already said. */

/** A build, as the Firmware tab lists it: a latest one, named
 *  `<project>_<catalogue>_latest` and fetched when used, or a saved one,
 *  named `<project>_<catalogue>_<stamp>`. What it plays and its kind are
 *  known once it is here. */
export interface DeviceRow {
  ref: string
  project?: string
  catalogue?: string
  stamp?: string
  name?: string
  virtual_hardware?: string | null
  virtual_radio?: string | null
  kind?: string
  /** A latest one: whether it has been fetched, and from where it comes. */
  fetched?: boolean
  source?: 'web' | 'builds' | 'compiled'
  error?: string
}

export interface GeodataInfo {
  name: string
  kind: 'pack' | 'synthetic'
  /** Latitude and longitude: a pack's centre, synthetic ground's 0°, 0°. */
  origin: [number, number]
  /** [lon0, lat0, lon1, lat1]. */
  bbox: [number, number, number, number]
  pack?: string
  crs_epsg?: number
  layers?: string[]
  /** Each source's notice, which the map shows when it draws this ground. */
  licences?: { source: string; notice: string }[]
  pack_manifest_hash?: string
  exponent?: number
  terrain?: string
  extent_m?: number
  /** How many nodesets have a node inside its bbox. */
  nodesets?: number
  error?: string
}

export interface NodesetRow {
  name: string
  nodes?: number
  bbox?: [number, number, number, number] | null
  tags?: Record<string, number>
  error?: string
}

/** A file a script imports that is SIMesh's own: the library's (read-only
 *  here) or another script's. */
export interface ScriptReference { name: string; path: string; library: boolean }

export interface ScriptRow {
  name: string; doc?: string; report?: boolean; references?: ScriptReference[]; error?: string
  /** The scripts that include or import this one: it is part of them, not run on its own. */
  included_by?: string[]
}

export interface ScriptRun {
  run: string
  name: string
  sim: string
  state: 'running' | 'stopping' | 'exited'
  code: number | null
  started: number
  /** The simulation's run directory, relative to testbed/. */
  run_dir?: string
  /** Whether that run has a report.md. */
  report?: boolean
  lines: string[]
}

/** A pack being built from its sources, as the front tells it. */
export interface BuildRow {
  name: string
  state: 'fetching' | 'compiling' | 'done' | 'failed' | 'cancelled'
  step: string | null
  done: number
  total: number
  /** Bytes fetched so far, and of how many (null: a host did not say). */
  fetched: number
  of: number | null
  error: string | null
  spec: { bbox: number[]; res_m: number; terrain: string; buildings: string; population: string }
}

export interface Listing { name: string; [key: string]: unknown }

/** The radio scripts/globals.py gives every node not tagged no-radio. */
export interface Globals { FREQ_MHZ: number; SF: number; BW_KHZ: number; CR: number }

const LINES_KEPT = 2000

export const useCatalog = defineStore('catalog', {
  state: () => ({
    latest: [] as DeviceRow[],
    saved: [] as DeviceRow[],
    arch: '' as string,
    antennas: [] as AntennaSpec[],
    geodata: [] as GeodataInfo[],
    /** The pack being built, or the last one that failed. */
    build: null as BuildRow | null,
    nodesets: [] as NodesetRow[],
    scripts: [] as ScriptRow[],
    snapshots: [] as Listing[],
    runs: {} as Record<string, ScriptRun>,
    /** scripts/globals.py's radio, as the registry and `store` say it; null
     *  with `globalsError` when the file does not give it. */
    globals: null as Globals | null,
    globalsError: null as string | null,
  }),

  getters: {
    runList: (s): ScriptRun[] => Object.values(s.runs).sort((a, b) => b.started - a.started),
    /** The antennas by type, for the patterns. */
    antennaByType: (s): Record<string, AntennaSpec> =>
      Object.fromEntries(s.antennas.map(a => [a.type, a])),
  },

  actions: {
    receive(msg: Record<string, unknown>) {
      if (msg.sim !== undefined) {
        if (msg.type === 'store') this.names(msg)
        return
      }
      if (msg.type === 'devices_changed') {
        void this.refreshDevices()
      } else if (msg.type === 'geodata_progress') {
        const row = msg.build as BuildRow | null
        this.build = row && row.state !== 'done' && row.state !== 'cancelled' ? row : null
        if (row?.state === 'done') void this.refreshGeodata()
      } else if (msg.type === 'sims') {
        this.names(msg)
        for (const r of (msg.script_runs as Omit<ScriptRun, 'lines'>[]) ?? []) {
          const had = this.runs[r.run]
          this.runs[r.run] = { ...r, lines: had?.lines ?? [] }
        }
      } else if (msg.type === 'store') {
        this.names(msg)
      } else if (msg.type === 'script_output') {
        const run = this.runs[msg.run as string]
        if (run) {
          run.lines.push(msg.line as string)
          if (run.lines.length > LINES_KEPT) run.lines.splice(0, run.lines.length - LINES_KEPT)
        }
      } else if (msg.type === 'script_exit') {
        const run = this.runs[msg.run as string]
        if (run) { run.state = 'exited'; run.code = msg.code as number }
      }
    },

    /** Names from the registry or a simulation's `store` message. */
    names(msg: Record<string, unknown>) {
      const keep = <T extends { name: string }>(old: T[], v: unknown): T[] => {
        if (!Array.isArray(v)) return old
        const byName = new Map(old.map(o => [o.name, o]))
        return (v as string[]).map(n => byName.get(n) ?? ({ name: n } as T))
      }
      this.geodata = keep(this.geodata, msg.geodata_names)
      this.nodesets = keep(this.nodesets, msg.nodesets)
      this.scripts = keep(this.scripts, msg.scripts)
      this.snapshots = keep(this.snapshots, msg.snapshots)
      if ('globals' in msg) {
        const g = (msg.globals as Globals | null) ?? null
        // Replaced only when it changes, so what is drawn from it is not redone every second.
        if (JSON.stringify(g) !== JSON.stringify(this.globals)) this.globals = g
        this.globalsError = (msg.globals_error as string | null) ?? null
      }
    },

    async refresh() {
      const [g, n, s, p, d, a] = await Promise.all([
        request('geodata_list'), request('nodeset_list'), request('script_list'),
        request('snapshot_list'), request('device_list'), request('antenna_list'),
      ])
      if (g.ok) { this.geodata = g.geodata as GeodataInfo[]; this.build = g.build as BuildRow | null }
      if (n.ok) this.nodesets = n.nodesets as NodesetRow[]
      if (s.ok) this.scripts = s.scripts as ScriptRow[]
      if (p.ok) this.snapshots = p.snapshots as Listing[]
      if (d.ok) this.takeDevices(d)
      if (a.ok) this.antennas = a.antennas as AntennaSpec[]
    },

    takeDevices(d: Record<string, unknown>) {
      this.latest = d.latest as DeviceRow[]
      this.saved = d.saved as DeviceRow[]
      this.arch = d.arch as string
    },

    /** The antennas, once: the catalogue is a file and does not change. A
     *  simd on its own sends it in its snapshot instead. */
    async ensureAntennas() {
      if (this.antennas.length || !useSocket().front) return
      const a = await request('antenna_list')
      if (a.ok) this.antennas = a.antennas as AntennaSpec[]
    },

    async refreshGeodata() {
      const g = await request('geodata_list')
      if (g.ok) { this.geodata = g.geodata as GeodataInfo[]; this.build = g.build as BuildRow | null }
    },

    async refreshDevices() {
      const d = await request('device_list')
      if (d.ok) this.takeDevices(d)
    },

    /** A script run's output so far, for a page opened after it began. */
    async loadRun(run: string) {
      const r = await request('script_log', { run })
      const held = this.runs[run]
      if (r.ok && held) held.lines = r.lines as string[]
    },
  },
})

/** Whether [lon0, lat0, lon1, lat1] boxes overlap. */
export function overlaps(a: number[] | null | undefined, b: number[] | null | undefined): boolean {
  if (!a || !b) return false
  return a[0]! <= b[2]! && b[0]! <= a[2]! && a[1]! <= b[3]! && b[1]! <= a[3]!
}
