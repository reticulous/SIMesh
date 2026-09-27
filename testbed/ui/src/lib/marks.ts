/* What the map is handed to draw, and what it hands back. */
import type { Footprint } from './planner'

/** One node on the map, from the nodeset being edited or a simulation's stations. */
export interface MapNode {
  name: string
  id: number
  lat: number
  lon: number
  height_m: number
  height_from: string
  tags: string[]
  /** Its declared role, and when it runs, the role its kind reads from it. */
  role?: string | null
  liveRole?: string | null
  status?: string
  stale?: boolean
}

/** One other node as heard from the selected one. */
export interface LinkMark {
  name: string
  /** dBm at that node. */
  level: number
  decodable: boolean
  /** Whether the first Fresnel zone is clear; null when not known. */
  los: boolean | null
}

/** A point on the ground the pointer was on. */
export interface GroundPoint {
  x: number
  y: number
  lat: number
  lon: number
  /** The footprint under the point, when the pack has building geometry. */
  footprint: Footprint | null
  /** Ground height there from the fetched tile, when there is one. */
  ground: number | null
}

/** How a click or a rectangle changes the selection. */
export type Pick = 'replace' | 'add' | 'toggle' | 'remove'
