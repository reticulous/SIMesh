import { defineStore } from 'pinia'
import { request } from '../lib/front'

/* What the store holds, for every picker and list on the page: devices,
 * geodata, nodesets, scripts, snapshots, and the script runs the front keeps
 * with their output. The registry the front sends every second names what
 * there is; the *_list verbs describe it, and a name alone keeps what a
 * description already said. */

export interface DeviceRow {
  ref: string
  name: string
  stands_for?: string | null
  kind?: string
  arch?: string
  stamp?: string
  catalogue: string
  local: boolean
  runs_here: boolean
  newest_of: string | null
  source?: string
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
  pack_manifest_hash?: string
  exponent?: number
  terrain?: string
  extent_m?: number
  error?: string
}

export interface NodesetRow {
  name: string
  nodes?: number
  bbox?: [number, number, number, number] | null
  tags?: Record<string, number>
  error?: string
}

export interface ScriptRow {
  name: string; doc?: string; setup?: boolean; main?: boolean; report?: boolean; error?: string
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

export interface Listing { name: string; [key: string]: unknown }

const LINES_KEPT = 2000

export const useCatalog = defineStore('catalog', {
  state: () => ({
    devices: [] as DeviceRow[],
    arch: '' as string,
    geodata: [] as GeodataInfo[],
    nodesets: [] as NodesetRow[],
    scripts: [] as ScriptRow[],
    snapshots: [] as Listing[],
    runs: {} as Record<string, ScriptRun>,
  }),

  getters: {
    runList: (s): ScriptRun[] => Object.values(s.runs).sort((a, b) => b.started - a.started),
  },

  actions: {
    receive(msg: Record<string, unknown>) {
      if (msg.sim !== undefined) {
        if (msg.type === 'store') this.names(msg)
        return
      }
      if (msg.type === 'devices_changed') {
        void this.refreshDevices()
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
    },

    async refresh() {
      const [g, n, s, p, d] = await Promise.all([
        request('geodata_list'), request('nodeset_list'), request('script_list'),
        request('snapshot_list'), request('device_list'),
      ])
      if (g.ok) this.geodata = g.geodata as GeodataInfo[]
      if (n.ok) this.nodesets = n.nodesets as NodesetRow[]
      if (s.ok) this.scripts = s.scripts as ScriptRow[]
      if (p.ok) this.snapshots = p.snapshots as Listing[]
      if (d.ok) { this.devices = d.devices as DeviceRow[]; this.arch = d.arch as string }
    },

    async refreshDevices() {
      const d = await request('device_list')
      if (d.ok) { this.devices = d.devices as DeviceRow[]; this.arch = d.arch as string }
    },

    /** A script run's output so far, for a page opened after it began. */
    async loadRun(run: string) {
      const r = await request('script_log', { run })
      const held = this.runs[run]
      if (r.ok && held) held.lines = r.lines as string[]
    },

    /** The device refs a node can name: the catalogues first, then every device. */
    deviceChoices(): { value: string; label: string }[] {
      const out: { value: string; label: string }[] = []
      for (const d of this.devices) {
        if (d.newest_of && d.runs_here) out.push({ value: d.newest_of, label: `${d.newest_of} (now ${d.name})` })
      }
      for (const d of this.devices) {
        if (d.runs_here && !d.error) out.push({ value: d.ref, label: d.name })
      }
      return out
    },
  },
})

/** Whether [lon0, lat0, lon1, lat1] boxes overlap. */
export function overlaps(a: number[] | null | undefined, b: number[] | null | undefined): boolean {
  if (!a || !b) return false
  return a[0]! <= b[2]! && b[0]! <= a[2]! && a[1]! <= b[3]! && b[1]! <= a[3]!
}
