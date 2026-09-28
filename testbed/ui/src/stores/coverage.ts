import { defineStore } from 'pinia'
import { request } from '../lib/front'
import { useSocket } from './socket'
import { useCatalog, type GeodataInfo } from './catalog'
import { threshold } from '../lib/links'
import { direction, gain, specOf, type Antenna, type AntennaSpec } from '../lib/antennas'
import { plannerBase, terrainAt, terrainGrid, type Grid } from '../lib/planner'
import { txDbm } from '../lib/boards'
import type { Frame } from '../lib/proj'
import { NO_RADIO, type NodeView } from './nodes'

/* Coverage: where each node's frames can be decoded, and the network's as
 * the best margin at each point over the nodes on show.
 *
 * On a pack each node's path loss to the ground around it is a raster the
 * front has the planner sweep (coverage.py), cached by the node's position
 * and height, fetched here once per key and kept for the page's life, with
 * the terrain under it on the same grid. On synthetic ground it is the
 * log-distance formula, worked out here, on flat ground at 0 m. Either way
 * the level at a point is the node's maximum power at its connector
 * (lib/boards.ts) plus its antenna's gain
 * toward the point less that loss, and the margin is that less the
 * decoding threshold (the SF and bandwidth scripts/globals.py gives every
 * radio, over the noise floor), as the ether rules. A node tagged no-radio
 * covers nothing, and without the globals nothing is covered. The gain is taken in three dimensions: the azimuth to the
 * point and the elevation from the antenna's tip (the ground under the node
 * plus its height) to a receiver RX_HEIGHT_M over the ground there, the
 * earth's curvature taken off. So a high collinear's narrow beam passes over
 * the ground close under it, and an aimed yagi covers what it faces. */

export interface Raster { w: number; h: number; ox: number; oy: number; rx: number; ry: number; loss: Uint16Array }

/** What the map asks at each point: the best margin in dB over the nodes, or null. */
export interface CoverageSource { version: string; marginAt(x: number, y: number): number | null }

const NEVER = 65535
const RX_GAIN_DBI = 0
/** The receiver the rasters are swept to (coverage.py RX_HEIGHT_M). */
const RX_HEIGHT_M = 2

/** What a margin over the decoding threshold is good for, from the most:
 *  indoors too (enough over the edge to lose a building's walls), outdoors
 *  only, and the edge, where fading decides; below 0 there is no chance and
 *  nothing is drawn. `from` is the least margin of each, in dB. */
export const EDGE_DB = 6
export const INDOOR_LOSS_DB = 15
export const COVERAGE_BANDS = [
  { name: 'indoors too', from: EDGE_DB + INDOOR_LOSS_DB, rgba: [34, 197, 94, 150] },
  { name: 'outdoors only', from: EDGE_DB, rgba: [250, 204, 21, 140] },
  { name: 'edge, maybe', from: 0, rgba: [239, 68, 68, 130] },
] as const

function readRaster(buf: ArrayBuffer): Raster | null {
  const dv = new DataView(buf)
  if (String.fromCharCode(dv.getUint8(0), dv.getUint8(1), dv.getUint8(2), dv.getUint8(3)) !== 'PLS2') return null
  const w = dv.getUint32(4, true), h = dv.getUint32(8, true)
  return {
    w, h, ox: dv.getFloat64(12, true), oy: dv.getFloat64(20, true),
    rx: dv.getFloat64(28, true), ry: dv.getFloat64(36, true),
    loss: new Uint16Array(buf.slice(44, 44 + w * h * 2)),
  }
}

/** One node's antenna: where it stands, the ground there and its tip, in
 *  metres above sea level, and its pattern. */
interface Beam { x: number; y: number; ground: number; top: number; spec: AntennaSpec | null; antenna: Antenna }

function beamGain(b: Beam, x: number, y: number, rxGround: number): number {
  if (!b.spec) return 0
  const [az, el] = direction(b.x, b.y, b.top, x, y, rxGround + RX_HEIGHT_M)
  return gain(b.spec, b.antenna, az, el)
}

/* ── composites ──
 * The best margin over a set of nodes, on one grid in the pack's metres at
 * the rasters' own cell size, over the union of their extents. Each node's
 * raster is merged in once, when it and its terrain are there: a set whose
 * rasters land one by one grows, it is not rebuilt. The last COMPOSITES_KEPT
 * sets are kept. */
const COMPOSITES_KEPT = 6

interface Each { key: string; budget: number; raster: Raster | null; terrain: Grid | null; beam: Beam }

class Composite {
  res = 0
  ox = 0
  oy = 0
  w = 0
  h = 0
  best: Float32Array = new Float32Array(0)
  merged = new Set<string>()

  /** Merge the rasters not yet in, growing the grid to hold them. */
  add(each: Each[]) {
    const fresh = each.filter(e => e.raster && e.terrain && !this.merged.has(e.key))
    if (!fresh.length) return
    const all = [...fresh.map(e => e.raster!)]
    const res = this.res || all[0]!.rx
    let minx = this.w ? this.ox : Infinity, maxy = this.h ? this.oy : -Infinity
    let maxx = this.w ? this.ox + this.w * res : -Infinity, miny = this.h ? this.oy - this.h * res : Infinity
    for (const r of all) {
      minx = Math.min(minx, r.ox); maxy = Math.max(maxy, r.oy)
      maxx = Math.max(maxx, r.ox + r.w * r.rx); miny = Math.min(miny, r.oy - r.h * r.ry)
    }
    const w = Math.ceil((maxx - minx) / res), h = Math.ceil((maxy - miny) / res)
    if (w !== this.w || h !== this.h || minx !== this.ox || maxy !== this.oy) {
      const grown = new Float32Array(w * h).fill(-Infinity)
      // Carry what is merged across into the larger grid.
      const dc = Math.round((this.ox - minx) / res), dr = Math.round((maxy - this.oy) / res)
      for (let row = 0; row < this.h; row++) {
        grown.set(this.best.subarray(row * this.w, (row + 1) * this.w), (row + dr) * w + dc)
      }
      Object.assign(this, { res, ox: minx, oy: maxy, w, h, best: grown })
    }
    for (const e of fresh) {
      const r = e.raster!, t = e.terrain!
      const dc = Math.round((r.ox - this.ox) / res), dr = Math.round((this.oy - r.oy) / res)
      for (let row = 0; row < r.h; row++) {
        const to = (row + dr) * this.w + dc
        const y = r.oy - row * r.ry
        for (let col = 0; col < r.w; col++) {
          const v = r.loss[row * r.w + col]!
          if (v === NEVER) continue
          const x = r.ox + col * r.rx
          const m = e.budget + beamGain(e.beam, x, y, terrainAt(t, x, y) ?? e.beam.ground) - v / 100
          if (m > this.best[to + col]!) this.best[to + col] = m
        }
      }
      this.merged.add(e.key)
    }
  }

  at(x: number, y: number): number | null {
    if (!this.w) return null
    // A raster's origin is its first cell's centre, as the rasters give it.
    const col = Math.round((x - this.ox) / this.res), row = Math.round((this.oy - y) / this.res)
    if (col < 0 || row < 0 || col >= this.w || row >= this.h) return null
    const m = this.best[row * this.w + col]!
    return m === -Infinity ? null : m
  }
}

const composites = new Map<string, Composite>()

function compositeFor(key: string, each: Each[]): Composite {
  let c = composites.get(key)
  if (c) composites.delete(key)       // to the back: the most recently used
  else c = new Composite()
  composites.set(key, c)
  while (composites.size > COMPOSITES_KEPT) composites.delete(composites.keys().next().value!)
  c.add(each)
  return c
}

/** Where a node stands, as far as its raster goes. */
function placeKey(n: NodeView) { return `${n.lat},${n.lon},${n.height_m}` }

/** The nodes with a radio: every one not tagged no-radio. */
function withRadio(nodes: NodeView[]) { return nodes.filter(n => !n.tags.includes(NO_RADIO)) }

function fspl1m(freqHz: number) {
  return 20 * Math.log10(4 * Math.PI * Math.max(freqHz, 1) / 299_792_458)
}

export const useCoverage = defineStore('coverage', {
  state: () => ({
    /** Rasters by key, as the front names them. */
    rasters: {} as Record<string, Raster>,
    /** The terrain on each raster's own grid, by the raster's key. */
    terrains: {} as Record<string, Grid>,
    /** Each node's raster key on `geodata`, with the position it was asked
     *  for: a key counts only while the node still stands there, so an
     *  answer that lands late, or one from before a move, is never misread.
     *  Answers add to it, node by node, whatever else was asked since. */
    keys: {} as Record<string, { key: string; at: string }>,
    /** The last ask, whose answer alone says what is still being swept. */
    asked: 0,
    geodata: null as string | null,
    /** Nodes whose raster is being swept. */
    pending: [] as string[],
    problem: null as string | null,
    version: 0,
  }),

  actions: {
    /** Have the rasters for these nodes on this geodata: cached ones at once,
     *  the rest as the front's sweeps land. Nothing to fetch on synthetic ground. */
    async ensure(gd: GeodataInfo | null, all: NodeView[]) {
      const ask = ++this.asked
      const nodes = withRadio(all)
      if (!gd || gd.kind !== 'pack' || !nodes.length || !useSocket().front) { this.pending = []; return }
      if (this.geodata !== gd.name) { this.keys = {}; this.geodata = gd.name }
      const at = Object.fromEntries(nodes.map(n => [n.name, placeKey(n)]))
      const r = await request('coverage', {
        geodata: gd.name,
        nodes: nodes.map(n => ({ name: n.name, lat: n.lat, lon: n.lon, height_m: n.height_m })),
      })
      if (this.geodata !== gd.name) return
      const latest = ask === this.asked
      if (!r.ok) { if (latest) this.problem = r.error ?? 'no coverage'; return }
      const tiles = r.tiles as { node: string; key: string; cached: boolean }[]
      for (const t of tiles) this.keys[t.node] = { key: t.key, at: at[t.node] ?? '' }
      if (latest) {
        this.problem = null
        this.pending = tiles.filter(t => !t.cached && !this.rasters[t.key]).map(t => t.node)
      }
      this.version++
      await Promise.all(tiles.filter(t => t.cached).map(t => this.fetch(gd.name, t.key)))
    },

    async fetch(geodata: string, key: string) {
      if (this.rasters[key]) return
      try {
        const r = await fetch(`/api/coverage?geodata=${encodeURIComponent(geodata)}&key=${key}`)
        const raster = r.ok ? readRaster(await r.arrayBuffer()) : null
        if (!raster) return
        // The terrain under it, on its own grid, for each cell's elevation.
        const box = {
          minx: raster.ox - raster.rx / 2, maxx: raster.ox + (raster.w - 0.5) * raster.rx,
          maxy: raster.oy + raster.ry / 2, miny: raster.oy - (raster.h - 0.5) * raster.ry,
        }
        try {
          this.terrains[key] = await terrainGrid(plannerBase(geodata), box, raster.w, raster.h)
        } catch {
          // Flat at the node's own ground, rather than no coverage at all.
          this.terrains[key] = { w: 0, h: 0, ox: 0, oy: 0, rx: 1, ry: 1, terrain: new Float32Array(0) }
        }
        this.rasters[key] = raster
        this.version++
      } catch { /* the node stays uncovered on the map */ }
    },

    receive(msg: Record<string, unknown>) {
      if (msg.type === 'coverage_tile') {
        this.pending = this.pending.filter(n => n !== msg.node)
        void this.fetch(msg.geodata as string, msg.key as string)
      } else if (msg.type === 'coverage_error') {
        this.pending = this.pending.filter(n => n !== msg.node)
        this.problem = msg.error as string
      }
    },

    /** The margin source for these nodes on this geodata, in its frame. */
    source(gd: GeodataInfo, frame: Frame, all: NodeView[], noiseFigureDb = 6): CoverageSource {
      const exponent = gd.exponent ?? 2.7
      const catalog = useCatalog()
      const antennas = catalog.antennaByType
      const radio = catalog.globals
      const nodes = radio ? withRadio(all) : []
      const radioKey = radio ? `${radio.FREQ_MHZ},${radio.SF},${radio.BW_KHZ}` : 'none'
      const each = nodes.map((n) => {
        const freq = radio!.FREQ_MHZ * 1e6
        const budget = txDbm(n.max_dbm, undefined) + RX_GAIN_DBI
          - threshold(radio!.BW_KHZ * 1e3, radio!.SF, noiseFigureDb)
        const [x, y] = frame.toXY(n.lat, n.lon)
        const anchor = fspl1m(freq)
        const held = this.geodata === gd.name ? this.keys[n.name] : undefined
        const key = held && held.at === placeKey(n) ? held.key : ''
        const raster = this.rasters[key] ?? null
        const terrain = this.terrains[key] ?? null
        const spec = specOf(antennas, n.antenna)
        const ground = terrain ? terrainAt(terrain, x, y) ?? 0 : 0
        const beam: Beam = { x, y, ground, top: ground + n.height_m, spec, antenna: n.antenna }
        // Past this the formula's loss exceeds the budget at the antenna's peak: nothing to ask.
        const reach = Math.pow(10, (budget + (spec?.peak_dbi ?? 0) - anchor) / (10 * exponent))
        return { x, y, budget, anchor, reach, key, raster, terrain, beam }
      })
      const pack = gd.kind === 'pack'
      const antennaKey = (n: NodeView) => `${JSON.stringify(n.antenna)}${n.max_dbm ?? ''}`
      const version = `${gd.name}|${radioKey}|${Object.keys(antennas).length}|${nodes.map((n, i) =>
        `${n.name}:${n.lat},${n.lon},${n.height_m},${antennaKey(n)}`
        + `:${each[i]!.key}:${each[i]!.raster && each[i]!.terrain ? 1 : 0}`).join(';')}`
      if (pack) {
        // One grid of the best margin over these nodes, kept per set of
        // nodes: a repaint is a lookup a cell, whatever the count, and
        // coming back to a set (the whole network after one node) is free.
        const grid = compositeFor(`${gd.name}|${radioKey}|${noiseFigureDb}|${Object.keys(antennas).length}|${nodes.map((n, i) =>
          `${n.name}:${each[i]!.key}:${antennaKey(n)}`).join(';')}`,
        each)
        return { version, marginAt: (x, y) => grid.at(x, y) }
      }
      return {
        version,
        marginAt(x: number, y: number): number | null {
          let best: number | null = null
          for (const e of each) {
            const d = Math.hypot(x - e.x, y - e.y)
            if (d > e.reach) continue
            const loss = e.anchor + 10 * exponent * Math.log10(Math.max(d, 1))
            const margin = e.budget + beamGain(e.beam, x, y, 0) - loss
            if (best === null || margin > best) best = margin
          }
          return best
        },
      }
    },
  },
})
