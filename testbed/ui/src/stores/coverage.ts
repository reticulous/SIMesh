import { defineStore } from 'pinia'
import { request } from '../lib/front'
import { useSocket } from './socket'
import { threshold } from '../lib/links'
import type { Frame } from '../lib/proj'
import type { GeodataInfo } from './catalog'
import type { NodeView } from './nodes'

/* Coverage: where each node's frames can be decoded, and the network's as
 * the best margin at each point over the nodes on show.
 *
 * On a pack each node's path loss to the ground around it is a raster the
 * front has the planner sweep (coverage.py), cached by the node's position
 * and height, fetched here once per key and kept for the page's life. On
 * synthetic ground it is the log-distance formula, worked out here. Either
 * way the level at a point is the node's transmit power plus its antenna
 * gain less that loss, and the margin is that less the node's own decoding
 * threshold (its SF and bandwidth, over the noise floor), as the ether rules. */

export interface Raster { w: number; h: number; ox: number; oy: number; rx: number; ry: number; loss: Uint16Array }

/** What the map asks at each point: the best margin in dB over the nodes, or null. */
export interface CoverageSource { version: string; marginAt(x: number, y: number): number | null }

const NEVER = 65535
const RX_GAIN_DBI = 0

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

/* ── composites ──
 * The best margin over a set of nodes, on one grid in the pack's metres at
 * the rasters' own cell size, over the union of their extents. Each node's
 * raster is merged in once, when it is there: a set whose rasters land one
 * by one grows, it is not rebuilt. The last COMPOSITES_KEPT sets are kept. */
const COMPOSITES_KEPT = 6

class Composite {
  res = 0
  ox = 0
  oy = 0
  w = 0
  h = 0
  best: Float32Array = new Float32Array(0)
  merged = new Set<string>()

  /** Merge the rasters not yet in, growing the grid to hold them. */
  add(each: { key: string; budget: number; raster: Raster | null }[]) {
    const fresh = each.filter(e => e.raster && !this.merged.has(e.key))
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
      const r = e.raster!
      const dc = Math.round((r.ox - this.ox) / res), dr = Math.round((this.oy - r.oy) / res)
      for (let row = 0; row < r.h; row++) {
        const to = (row + dr) * this.w + dc
        for (let col = 0; col < r.w; col++) {
          const v = r.loss[row * r.w + col]!
          if (v === NEVER) continue
          const m = e.budget - v / 100
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

function compositeFor(key: string, each: { key: string; budget: number; raster: Raster | null }[]): Composite {
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

function fspl1m(freqHz: number) {
  return 20 * Math.log10(4 * Math.PI * Math.max(freqHz, 1) / 299_792_458)
}

export const useCoverage = defineStore('coverage', {
  state: () => ({
    /** Rasters by key, as the front names them. */
    rasters: {} as Record<string, Raster>,
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
    async ensure(gd: GeodataInfo | null, nodes: NodeView[]) {
      const ask = ++this.asked
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
        if (raster) { this.rasters[key] = raster; this.version++ }
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
    source(gd: GeodataInfo, frame: Frame, nodes: NodeView[], noiseFigureDb = 6): CoverageSource {
      const exponent = gd.exponent ?? 2.7
      const each = nodes.map((n) => {
        const radio = n.radio ?? {}
        const freq = (radio.freq_mhz ?? 869.525) * 1e6
        const budget = (radio.tx_dbm ?? 14) + (n.antenna?.gain_dbi ?? 0) + RX_GAIN_DBI
          - threshold((radio.bw_khz ?? 125) * 1e3, radio.sf ?? 8, noiseFigureDb)
        const [x, y] = frame.toXY(n.lat, n.lon)
        const anchor = fspl1m(freq)
        // Past this the formula's loss exceeds the budget: nothing to ask.
        const reach = Math.pow(10, (budget - anchor) / (10 * exponent))
        const held = this.geodata === gd.name ? this.keys[n.name] : undefined
        const key = held && held.at === placeKey(n) ? held.key : ''
        return { x, y, budget, anchor, reach, key, raster: this.rasters[key] ?? null }
      })
      const pack = gd.kind === 'pack'
      const version = `${gd.name}|${nodes.map((n, i) =>
        `${n.name}:${n.lat},${n.lon},${n.height_m},${n.antenna?.gain_dbi},${JSON.stringify(n.radio)}`
        + `:${each[i]!.key}:${each[i]!.raster ? 1 : 0}`).join(';')}`
      if (pack) {
        // One grid of the best margin over these nodes, kept per set of
        // nodes: a repaint is a lookup a cell, whatever the count, and
        // coming back to a set (the whole network after one node) is free.
        const grid = compositeFor(`${gd.name}|${noiseFigureDb}|${nodes.map((n, i) =>
          `${n.name}:${each[i]!.key}:${n.antenna?.gain_dbi},${JSON.stringify(n.radio)}`).join(';')}`,
          each)
        return { version, marginAt: (x, y) => grid.at(x, y) }
      }
      return {
        // Which raster each node is drawn from is part of what was painted:
        // one arriving, or a node's key landing, is a new picture.
        version,
        marginAt(x: number, y: number): number | null {
          let best: number | null = null
          for (const e of each) {
            let loss: number
            if (pack) {
              const r = e.raster
              if (!r) continue
              const col = Math.round((x - r.ox) / r.rx), row = Math.round((r.oy - y) / r.ry)
              if (col < 0 || row < 0 || col >= r.w || row >= r.h) continue
              const v = r.loss[row * r.w + col]!
              if (v === NEVER) continue
              loss = v / 100
            } else {
              const d = Math.hypot(x - e.x, y - e.y)
              if (d > e.reach) continue
              loss = e.anchor + 10 * exponent * Math.log10(Math.max(d, 1))
            }
            const margin = e.budget - loss
            if (best === null || margin > best) best = margin
          }
          return best
        },
      }
    },
  },
})
