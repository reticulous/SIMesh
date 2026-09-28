import { defineStore } from 'pinia'
import { sample } from '../lib/planner'
import { useGeodata } from './geodata'

/* The ground under each node, in metres above sea level, for the antennas'
 * directions: 0 on synthetic ground; on a pack the terrain there, asked of
 * the sidecar once per point, a few at a time, and kept for the page's life.
 * `version` moves as answers land, so whatever reads them redraws. */

const AT_ONCE = 4

export const useGrounds = defineStore('grounds', {
  state: () => ({
    values: {} as Record<string, number>,
    version: 0,
  }),

  actions: {
    /** The ground at a node's position, or null while it is being asked. */
    at(lat: number, lon: number): number | null {
      const g = useGeodata()
      if (!g.isPack || !g.sidecar) return 0
      const key = `${g.current!.name}|${lat},${lon}`
      if (key in this.values) return this.values[key]!
      queue(key, g.sidecar, ...g.frame.toXY(lat, lon))
      return null
    },
  },
})

const wanted: { key: string; base: string; x: number; y: number }[] = []
const asked = new Set<string>()
let running = 0

function queue(key: string, base: string, x: number, y: number) {
  if (asked.has(key)) return
  asked.add(key)
  wanted.push({ key, base, x, y })
  pump()
}

function pump() {
  while (running < AT_ONCE && wanted.length) {
    const job = wanted.shift()!
    running++
    void sample(job.base, job.x, job.y)
      .then((s) => {
        const store = useGrounds()
        store.values[job.key] = Number.isFinite(s.ground) ? s.ground : 0
        store.version++
      })
      .catch(() => { asked.delete(job.key) })
      .finally(() => { running--; pump() })
  }
}
