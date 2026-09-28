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
  /** When it runs, the role its kind reads from it. */
  liveRole?: string | null
  status?: string
  stale?: boolean
}

/** The roles of a node that carries others' traffic, as tags and as a kind reads them. */
export const FORWARDING = ['transport', 'router', 'repeater']

/** A node's forwarding role: the live one when it runs, else the first
 *  forwarding tag it carries; null for none. */
export function forwardingRole(n: MapNode): string | null {
  if (n.liveRole) return FORWARDING.includes(n.liveRole) ? n.liveRole : null
  return FORWARDING.find(r => n.tags.includes(r)) ?? null
}

/** A node of another shown layer: drawn, named on hover, and a click on it
 *  makes its layer the active one. */
export interface OtherNode {
  layer: string
  name: string
  lat: number
  lon: number
  colour: string
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
