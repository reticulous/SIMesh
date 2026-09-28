import { defineStore } from 'pinia'
import { packFrame, syntheticFrame, type Frame } from '../lib/proj'
import { packInfo, plannerBase, type PackInfo } from '../lib/planner'
import { request } from '../lib/front'
import type { GeodataInfo } from './catalog'

/* The geodata on show: which one, and for a pack its manifest as the sidecar
 * reports it and the sidecar's base URL. It is the ground the map draws and
 * the frame every position on it is converted through.
 *
 * The map shows one geodata at a time, the one of whatever tab is on show:
 * the Geodata tab's preview, the Nodes tab's ground, or the attached
 * simulation's. `open` asks the front for it, which holds a pack's sidecar
 * for this page; the pack's own description is then fetched from the
 * sidecar. */

export const useGeodata = defineStore('geodata', {
  state: () => ({
    current: null as GeodataInfo | null,
    /** The sidecar's /api/pack for a pack, once it has answered. */
    pack: null as PackInfo | null,
    /** Why a pack has no ground on show, when it has none. */
    problem: null as string | null,
    /** The last view of each map that shares one, by its key (per geodata):
     *  metres at the centre and metres per pixel. */
    views: {} as Record<string, { cx: number; cy: number; mpp: number }>,
  }),

  getters: {
    isPack: (s): boolean => s.current?.kind === 'pack',
    /** The sidecar's URL prefix, for a pack. */
    sidecar: (s): string | null => (s.current?.kind === 'pack' ? plannerBase(s.current.name) : null),
    /** Metres east and north, and back: the pack's CRS, or synthetic ground's. */
    frame: (s): Frame => {
      const g = s.current
      if (g?.kind === 'pack' && g.crs_epsg) {
        const f = packFrame(g.crs_epsg)
        if (f) return f
      }
      return syntheticFrame
    },
  },

  actions: {
    /** Open a geodata by name on the front, which holds its sidecar, and show it. */
    async open(name: string | null): Promise<string | null> {
      if (!name) { await this.show(null); return null }
      if (this.current?.name === name && !this.problem) return null
      const r = await request('geodata_open', { name })
      if (!r.ok) {
        this.problem = r.error ?? 'could not open it'
        return this.problem
      }
      await this.show(r.geodata as GeodataInfo)
      return null
    },

    /** Put a geodata on the map. The same geodata again changes nothing. */
    async show(info: GeodataInfo | null) {
      const same = info && this.current && info.name === this.current.name
        && info.kind === this.current.kind && info.pack === this.current.pack
      if (same) { this.current = info; return }
      this.current = info
      this.pack = null
      this.problem = null
      if (!info || info.kind !== 'pack') return
      if (info.crs_epsg && !packFrame(info.crs_epsg)) {
        this.problem = `EPSG:${info.crs_epsg} is not a UTM zone, which is the only kind of pack CRS the map projects`
      }
      try {
        const pack = await packInfo(plannerBase(info.name))
        if (this.current?.name === info.name) this.pack = pack
      } catch (e) {
        if (this.current?.name === info.name) this.problem = (e as Error).message
      }
    },

    reconnected() {
      // A new socket holds no sidecar: open the geodata again for the map.
      const name = this.current?.name
      if (name) { this.current = null; void this.open(name) }
    },
  },
})
