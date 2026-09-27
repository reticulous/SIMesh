import { defineStore } from 'pinia'
import { request } from '../lib/front'
import { readTable, type LossTable } from '../lib/slt'
import { useSim, type LossProgress } from './sim'
import { useCatalog } from './catalog'

/* The Nodes tab: the nodeset on its geodata, the selection, and every edit.
 *
 * Standalone, the nodeset being edited is the page's own until it is saved:
 * every edit is made here, and Save sends the whole file's mapping to the
 * front (`nodeset_save {name, data}`), which checks it and writes it.
 * Attached to a running simulation, what is shown is that run's nodeset, as
 * its simd streams it, and every edit is a message to that simd, which
 * changes the run's own copy; Save as keeps it as a nodeset of the store.
 * Either way the page reads one list of nodes (`list`) and edits through one
 * set of actions, so the map, the editor and the tags panel do not care which. */

export type HeightFrom = 'measured' | 'roof' | 'raster' | 'assumed'
export type Role = 'transport' | 'client'

/** A node's declared radio, as the nodeset spells it. */
export interface Radio {
  freq_mhz?: number
  sf?: number
  bw_khz?: number
  cr?: number
  tx_dbm?: number
  sync?: number
  preamble?: number
}

/** One node as a nodeset file holds it. */
export interface NodeRecord {
  id: number
  lat: number
  lon: number
  height_m: number
  height_from: HeightFrom
  antenna: { gain_dbi: number }
  device: string
  role: Role | null
  radio: Radio | null
  tags: string[]
}

export interface Offset { between: [string, string]; db: number; note?: string }

/** A nodeset as the front and simd describe it (nodeset.Nodeset.as_dict). */
export interface NodesetData {
  name: string | null
  dirty: boolean
  geometry_hash?: string
  nodes: Record<string, NodeRecord>
  offsets: Offset[]
}

/** One node as the page shows it: its record, and when a simulation runs it, how it is. */
export interface NodeView extends NodeRecord {
  name: string
  live: boolean
  status?: string
  liveRole?: string | null
  stale?: boolean
  kind?: string | null
  web?: boolean
  deviceName?: string | null
  freq?: number
  sf?: number
  bw?: number
}

/** The fields an edit can change, flat; an empty role or radio takes it away. */
export interface NodeFields {
  id?: number
  lat?: number
  lon?: number
  height_m?: number
  height_from?: HeightFrom
  gain_dbi?: number
  device?: string
  role?: Role | ''
  radio?: Radio
  tags?: string[]
}

export type SelectMode = 'replace' | 'add' | 'toggle' | 'remove'

export const DEFAULT_DEVICE = 'stable'
export const DEFAULT_RADIO: Radio = { freq_mhz: 869.525, sf: 8, bw_khz: 125, cr: 5, tx_dbm: 14 }

/** What a new node runs: `stable` when there is a stable device, else the
 *  first catalogue that has one for this machine, else `stable` all the same. */
export function defaultDevice(): string {
  const newest = useCatalog().devices.filter(d => d.newest_of && d.runs_here).map(d => d.newest_of!)
  return newest.includes(DEFAULT_DEVICE) ? DEFAULT_DEVICE : newest[0] ?? DEFAULT_DEVICE
}

/** Before a simulation is started from a nodeset by name: when it is the
 *  one the Nodes tab is editing, with changes not saved, save them, since a
 *  simulation runs the file. The error, or null. */
export async function saveIfEditing(name: string | null): Promise<string | null> {
  const nodes = useNodes()
  if (!name || nodes.attached || nodes.nodeset?.name !== name || !nodes.nodeset.dirty) return null
  return await nodes.save()
}

export const useNodes = defineStore('nodes', {
  state: () => ({
    nodeset: null as NodesetData | null,
    /** The geodata the Nodes tab stands on, by name. */
    geodata: null as string | null,
    selection: [] as string[],
    /** The two nodes the pair inspector shows. */
    pair: null as [string, string] | null,
    losses: null as LossProgress | null,
    /** The nodeset's loss table, when one was computed for it as it stands. */
    table: null as LossTable | null,
  }),

  getters: {
    attached: (): boolean => useSim().attached,
    /** The nodeset on show: the run's when attached, else the one being edited. */
    data(): NodesetData | null { return this.attached ? useSim().nodeset : this.nodeset },
    open(): boolean { return this.data !== null },
    dirty(): boolean { return this.data?.dirty ?? false },
    list(): NodeView[] {
      const sim = useSim()
      if (this.attached) {
        const records = sim.nodeset?.nodes ?? {}
        return sim.nodeList.map(n => ({
          ...(records[n.name] ?? {
            id: n.id, lat: n.lat, lon: n.lon, height_m: n.height_m, height_from: n.height_from,
            antenna: { gain_dbi: n.gain_dbi }, device: n.device, role: null, radio: null, tags: n.tags,
          }),
          id: n.id, lat: n.lat, lon: n.lon, height_m: n.height_m,
          role: (n.declared_role as Role | null) ?? null, radio: n.declared_radio ?? null,
          tags: n.tags ?? [], name: n.name, live: true, status: n.status, liveRole: n.role,
          stale: n.stale, kind: n.kind, web: n.web, deviceName: n.device_name,
          freq: n.freq, sf: n.sf, bw: n.bw,
        }))
      }
      return Object.entries(this.nodeset?.nodes ?? {})
        .map(([name, r]) => ({ ...r, name, live: false }))
        .sort((a, b) => a.id - b.id)
    },
    byName(): Record<string, NodeView> {
      return Object.fromEntries(this.list.map(n => [n.name, n]))
    },
    names(): string[] { return this.list.map(n => n.name) },
    /** Every tag with how many nodes carry it. */
    tags(): [string, number][] {
      const count = new Map<string, number>()
      for (const n of this.list) for (const t of n.tags) count.set(t, (count.get(t) ?? 0) + 1)
      return [...count.entries()].sort((a, b) => a[0].localeCompare(b[0]))
    },
    offsets(): Offset[] { return this.data?.offsets ?? [] },
    /** The lowest id no node has. */
    nextId(): number {
      const taken = new Set(this.list.map(n => n.id))
      let id = 1
      while (taken.has(id)) id++
      return id
    },
    /** The loss table the links layer reads: the run's, or the one computed here. */
    linkTable(): LossTable | null { return this.attached ? useSim().table : this.table },
  },

  actions: {
    /* ── selection ── */
    select(name: string | null, mode: SelectMode = 'replace') {
      if (name === null) { if (mode === 'replace') this.selection = []; return }
      this.selectMany([name], mode)
    },
    selectMany(names: string[], mode: SelectMode = 'replace') {
      const now = new Set(mode === 'replace' ? [] : this.selection)
      for (const n of names) {
        if (mode === 'remove') now.delete(n)
        else if (mode === 'toggle' && now.has(n)) now.delete(n)
        else now.add(n)
      }
      this.selection = this.names.filter(n => now.has(n))
    },
    selectTag(tag: string, mode: 'add' | 'remove') {
      this.selectMany(this.list.filter(n => n.tags.includes(tag)).map(n => n.name), mode)
    },

    /* ── edits: the run's when attached, else the page's own ── */
    touched(geometry: boolean) {
      if (!this.nodeset) return
      this.nodeset.dirty = true
      if (geometry) {
        this.table = null
        if (this.losses?.error) this.losses = null
      }
    },

    /** A nodeset to edit: the one open, or a new unsaved one when none is. */
    ensureOpen() {
      if (!this.attached && !this.nodeset) this.adopt({ name: null, dirty: false, nodes: {}, offsets: [] })
    },

    place(name: string, lat: number, lon: number, fields: NodeFields = {}): boolean {
      this.ensureOpen()
      if (this.byName[name]) return false
      const record = {
        device: fields.device ?? defaultDevice(), radio: fields.radio ?? { ...DEFAULT_RADIO },
        ...(fields.role ? { role: fields.role } : {}),
        ...(fields.height_m !== undefined ? { height_m: fields.height_m } : {}),
        ...(fields.height_from ? { height_from: fields.height_from } : {}),
        ...(fields.tags ? { tags: fields.tags } : {}),
      }
      if (this.attached) {
        useSim().addNode(name, lat, lon, record)
      } else {
        if (!this.nodeset) return false
        this.nodeset.nodes[name] = {
          id: fields.id ?? this.nextId, lat, lon, height_m: fields.height_m ?? 2,
          height_from: fields.height_from ?? 'assumed', antenna: { gain_dbi: fields.gain_dbi ?? 0 },
          device: record.device, role: (fields.role || null) as Role | null,
          radio: record.radio, tags: fields.tags ?? [],
        }
        this.touched(true)
      }
      this.selection = [name]
      return true
    },

    move(name: string, lat: number, lon: number, settle: boolean) {
      if (this.attached) { useSim().moveNode(name, lat, lon, settle); return }
      const n = this.nodeset?.nodes[name]
      if (!n || !settle) return
      n.lat = lat
      n.lon = lon
      this.touched(true)
    },

    /** The same fields on every node named; a field not given is left alone. */
    setMany(names: string[], fields: NodeFields) {
      if (!Object.keys(fields).length) return
      if (this.attached) {
        for (const name of names) useSim().setNode(name, fields as Record<string, unknown>)
        return
      }
      const geometry = fields.lat !== undefined || fields.lon !== undefined || fields.height_m !== undefined
      for (const name of names) {
        const n = this.nodeset?.nodes[name]
        if (!n) continue
        const { gain_dbi, role, radio, ...rest } = fields
        Object.assign(n, rest)
        if (gain_dbi !== undefined) n.antenna = { gain_dbi }
        if (role !== undefined) n.role = role || null
        if (radio !== undefined) n.radio = Object.keys(radio).length ? { ...radio } : null
      }
      this.touched(geometry)
    },

    /** Some radio figures on every node named, each keeping its others. */
    setRadio(names: string[], change: Radio) {
      for (const name of names) {
        const now = this.byName[name]?.radio ?? {}
        this.setMany([name], { radio: { ...now, ...change } })
      }
    },

    /** A tag onto every node named, or off it, their other tags untouched. */
    tag(names: string[], tag: string, on: boolean) {
      for (const name of names) {
        const now = this.byName[name]?.tags ?? []
        const next = on ? (now.includes(tag) ? now : [...now, tag]) : now.filter(t => t !== tag)
        if (next !== now && next.join() !== now.join()) this.setMany([name], { tags: next })
      }
    },

    /** A node's name is its reference everywhere, so a rename carries its offsets. */
    rename(name: string, to: string): boolean {
      const ns = this.nodeset
      if (this.attached || !ns?.nodes[name] || ns.nodes[to] || !to) return false
      const nodes: Record<string, NodeRecord> = {}
      for (const [k, v] of Object.entries(ns.nodes)) nodes[k === name ? to : k] = v
      ns.nodes = nodes
      for (const o of ns.offsets) o.between = o.between.map(n => (n === name ? to : n)) as [string, string]
      this.selection = this.selection.map(n => (n === name ? to : n))
      if (this.pair) this.pair = this.pair.map(n => (n === name ? to : n)) as [string, string]
      this.touched(true)
      return true
    },

    remove(names: string[]) {
      if (this.attached) { for (const n of names) useSim().removeNode(n) }
      else if (this.nodeset) {
        for (const n of names) delete this.nodeset.nodes[n]
        this.nodeset.offsets = this.nodeset.offsets.filter(o => !o.between.some(e => names.includes(e)))
        this.touched(true)
      }
      this.selection = this.selection.filter(n => !names.includes(n))
      if (this.pair?.some(n => names.includes(n))) this.pair = null
    },

    /** The dB added between two nodes; 0 removes it. */
    setOffset(a: string, b: string, db: number, note = '') {
      if (this.attached) { useSim().setOffset(a, b, db, note); return }
      const ns = this.nodeset
      if (!ns) return
      const rest = ns.offsets.filter(o =>
        !((o.between[0] === a && o.between[1] === b) || (o.between[0] === b && o.between[1] === a)))
      ns.offsets = db ? [...rest, { between: [a, b], db, ...(note ? { note } : {}) }] : rest
      this.touched(false)
    },

    /* ── files, through the front ── */
    adopt(data: NodesetData) {
      for (const n of Object.values(data.nodes)) {
        n.antenna = { gain_dbi: n.antenna?.gain_dbi ?? 0 }
        n.tags = n.tags ?? []
        n.device = n.device ?? DEFAULT_DEVICE
        n.role = n.role ?? null
        n.radio = n.radio ?? null
      }
      data.offsets = data.offsets ?? []
      this.nodeset = data
      this.selection = []
      this.pair = null
      this.table = null
      this.losses = null
    },

    /** The nodeset file's mapping, as nodeset_save takes it. */
    fileData(): Record<string, unknown> {
      const d = this.data!
      return { nodes: d.nodes, offsets: d.offsets }
    },

    async openNodeset(name: string): Promise<string | null> {
      const r = await request('nodeset_open', { name })
      if (!r.ok) return r.error ?? 'could not open it'
      this.adopt(r.nodeset as NodesetData)
      return null
    },
    async newNodeset(name: string): Promise<string | null> {
      const r = await request('nodeset_new', { name })
      if (!r.ok) return r.error ?? 'could not make it'
      this.adopt(r.nodeset as NodesetData)
      return null
    },
    async save(): Promise<string | null> {
      if (this.attached) return 'a running simulation\'s nodeset is kept with Save as'
      if (!this.nodeset?.name) return 'the nodeset has no name yet: Save as'
      return this.saveAs(this.nodeset.name, false)
    },
    async saveAs(name: string, fresh = true): Promise<string | null> {
      if (!this.data) return 'no nodeset open'
      const r = await request(fresh ? 'nodeset_save_as' : 'nodeset_save', { name, data: this.fileData() })
      if (!r.ok) return r.error ?? 'could not save it'
      if (!this.attached && this.nodeset) {
        const saved = r.nodeset as NodesetData
        this.nodeset.name = saved.name
        this.nodeset.dirty = false
        this.nodeset.geometry_hash = saved.geometry_hash
      }
      return null
    },
    close() {
      this.nodeset = null
      this.selection = []
      this.pair = null
      this.table = null
    },
    async importCsv(format: 'sites' | 'nodes', text: string, name: string,
                    heightM: number | null, device: string | null): Promise<string | null> {
      const r = await request('nodeset_import', {
        name, format, text, ...(heightM ? { height_m: heightM } : {}), ...(device ? { device } : {}),
      })
      if (!r.ok) return r.error ?? 'could not import it'
      this.adopt(r.nodeset as NodesetData)
      return null
    },

    /** The saved nodeset's loss table for one band, computed or from the cache. */
    async computeLosses(band = '868'): Promise<string | null> {
      const ns = this.nodeset
      if (!ns?.name || ns.dirty) return 'save the nodeset first: the table is computed from the saved file'
      if (!this.geodata) return 'choose the geodata first'
      this.losses = { band, done: 0, total: 0, running: true }
      const r = await request('losses_compute', { geodata: this.geodata, nodeset: ns.name, bands: [band] })
      if (!r.ok) {
        this.losses = { band, done: 0, total: 0, running: false, error: r.error ?? 'failed' }
        return r.error ?? 'the table could not be computed'
      }
      const table = (r.tables as Record<string, { path: string; cached: boolean }>)[band]
      this.losses = { band, done: this.losses?.total ?? 0, total: this.losses?.total ?? 0,
                      running: false, cached: table?.cached }
      if (table) await this.loadTable(table.path)
      return null
    },

    async loadTable(path: string) {
      try {
        const r = await fetch(`/api/table?path=${encodeURIComponent(path)}`)
        if (r.ok) this.table = readTable(await r.arrayBuffer())
      } catch { /* the links layer stays off */ }
    },

    receive(msg: Record<string, unknown>) {
      if (msg.type === 'losses_progress' && msg.nodeset !== undefined && msg.sim === undefined) {
        this.losses = {
          band: msg.band as string, done: msg.done as number, total: msg.total as number,
          running: true,
        }
      }
    },
  },
})
