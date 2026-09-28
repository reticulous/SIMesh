/* Antennas: gain by direction, as testbed/antennas.py has it.
 *
 * A pattern is a few figures from the catalogue (antennas/catalogue.yaml):
 * the loss below the peak is 12·((el − tilt)/vbw)², plus for a directional
 * antenna 12·(az/hbw)², together never more than the floor. Each term is
 * 3 dB at half its beamwidth. A directional antenna is aimed by its node
 * (azimuth clockwise from grid north, elevation above the horizon); an omni
 * is the same all round. Between two antennas the direction is the line
 * between their tips, the far one dropped by the earth's curvature at the
 * effective radius (k = 4/3). */

export interface AntennaSpec {
  type: string
  label: string
  description: string
  kind: 'omni' | 'directional'
  peak_dbi: number
  vbw_deg: number
  tilt_deg: number
  hbw_deg: number | null
  floor_db: number
  svg: string | null
}

/** A node's antenna, as the nodeset has it. */
export interface Antenna { type: string; azimuth_deg?: number; elevation_deg?: number }

export const DEFAULT_ANTENNA = 'whip_sma_quarter_wave'
const EARTH_RADIUS_M = 6371008.8
const K_FACTOR = 4 / 3

function wrap180(deg: number) { return ((deg + 180) % 360 + 360) % 360 - 180 }

/** Gain in dBi toward azimuth `az` (clockwise from grid north) and elevation `el`. */
export function gain(spec: AntennaSpec, antenna: Antenna | null | undefined, az: number, el: number): number {
  let tilt = spec.tilt_deg
  let off = 0
  if (spec.kind === 'directional') {
    tilt += antenna?.elevation_deg ?? 0
    off = wrap180(az - (antenna?.azimuth_deg ?? 0))
  }
  let down = 12 * ((el - tilt) / spec.vbw_deg) ** 2
  if (spec.kind === 'directional' && spec.hbw_deg) down += 12 * (off / spec.hbw_deg) ** 2
  return spec.peak_dbi - Math.min(down, spec.floor_db)
}

/** Azimuth and elevation in degrees of the line from one antenna tip to another:
 *  positions in a projected frame's metres (x east, y north), tops above sea level. */
export function direction(ax: number, ay: number, aTop: number, bx: number, by: number, bTop: number): [number, number] {
  const dx = bx - ax, dy = by - ay
  const d = Math.hypot(dx, dy)
  const az = ((Math.atan2(dx, dy) * 180 / Math.PI) % 360 + 360) % 360
  const drop = d * d / (2 * K_FACTOR * EARTH_RADIUS_M)
  const el = Math.atan2(bTop - aTop - drop, Math.max(d, 1e-6)) * 180 / Math.PI
  return [az, el]
}

/** The catalogue's entry for a node's antenna, the default one for a type it lacks. */
export function specOf(catalogue: Record<string, AntennaSpec>, antenna: Antenna | null | undefined): AntennaSpec | null {
  return catalogue[antenna?.type ?? DEFAULT_ANTENNA] ?? catalogue[DEFAULT_ANTENNA] ?? null
}

export interface End { x: number; y: number; top: number; antenna: Antenna | null | undefined }

/** What a pair's two antennas add to it, each toward the other; 0 without a catalogue. */
export function pairGain(catalogue: Record<string, AntennaSpec>, a: End, b: End): number {
  const sa = specOf(catalogue, a.antenna), sb = specOf(catalogue, b.antenna)
  if (!sa || !sb) return 0
  const [azAB, elAB] = direction(a.x, a.y, a.top, b.x, b.y, b.top)
  const [azBA, elBA] = direction(b.x, b.y, b.top, a.x, a.y, a.top)
  return gain(sa, a.antenna, azAB, elAB) + gain(sb, b.antenna, azBA, elBA)
}

/** A short line saying what an antenna is and how it is aimed. */
export function antennaText(catalogue: Record<string, AntennaSpec>, antenna: Antenna | null | undefined): string {
  const spec = specOf(catalogue, antenna)
  if (!spec) return antenna?.type ?? '—'
  const aim = spec.kind === 'directional'
    ? `, aimed ${Math.round(antenna?.azimuth_deg ?? 0)}° / ${Math.round(antenna?.elevation_deg ?? 0)}°` : ''
  return `${spec.label} (${spec.peak_dbi} dBi${aim})`
}
