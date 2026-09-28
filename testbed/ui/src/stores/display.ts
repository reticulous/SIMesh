import { defineStore } from 'pinia'
import type { BaseLayer } from '../lib/planner'

/* How the map is shown, per tab: the Geodata tab's preview and the Nodes
 * tab keep their own choices, kept per browser. The ground's are the pack's
 * layers; the rest are the nodes' own and the Geodata tab has none of them. */

export type View = 'geodata' | 'nodes'

export interface Display {
  base: BaseLayer
  roads: boolean
  buildings: boolean
  /** Synthetic ground's grid labels: metres, or degrees. */
  units: 'metres' | 'degrees'
  /** A pack's population, as a heatmap. One heatmap at a time: population
   *  and coverage exclude each other, and under either the rest goes grey. */
  population: boolean
  coverage: boolean
  offsets: boolean
  labels: boolean
  tags: boolean
  heights: boolean
}

const DEFAULTS: Display = {
  base: 'terrain', roads: true, buildings: true, units: 'metres', population: false,
  coverage: true, offsets: true, labels: true, tags: true, heights: true,
}
const BASES: Display['base'][] = ['terrain', 'clutter']

function key(view: View) { return `simesh.display.${view}` }

function load(view: View): Display {
  const out = { ...DEFAULTS }
  try { Object.assign(out, JSON.parse(localStorage.getItem(key(view)) ?? '{}')) } catch { /* none kept */ }
  if (!BASES.includes(out.base)) out.base = 'terrain'
  if (out.population && out.coverage) out.coverage = false
  return out
}

/** Whether a heatmap is on show, which turns the rest of the map grey. */
export function heatmapOn(d: Display, coverageShown: boolean): boolean {
  return d.population || (d.coverage && coverageShown)
}

export const useDisplay = defineStore('display', {
  state: () => ({
    geodata: load('geodata'), nodes: load('nodes'),
    /** The menu is open: what sits under its corner of the map moves aside. */
    menuOpen: false,
  }),

  actions: {
    set(view: View, change: Partial<Display>) {
      Object.assign(this[view], change)
      // One heatmap at a time: the one just turned on wins.
      if (change.population) this[view].coverage = false
      if (change.coverage) this[view].population = false
      try { localStorage.setItem(key(view), JSON.stringify(this[view])) } catch { /* private window */ }
    },
  },
})
