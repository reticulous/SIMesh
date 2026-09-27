/* What every other node would hear from one, from the loss table, by the
 * ether's own rule (ether/ether.py): the level is transmit power plus both
 * antenna gains less the path loss, corrected from the table's centre to the
 * carrier by 20·log10(f/f0); a frame is decodable at or above the noise floor
 * (kTB plus the noise figure) plus its spreading factor's demodulation
 * threshold, and anything weaker still counts towards the receiver's
 * interference. Those are drawn down to 10 dB under the floor, where their
 * share of a sum stops mattering. */
import { cell, FLAG_LOS_CLEAR, type LossTable } from './slt'
import type { LinkMark } from './marks'

export const DEFAULT_POWER_DBM = 14
const THERMAL_DBM_PER_HZ = -174
const SENSITIVITY_DB: Record<number, number> = {
  5: -2.5, 6: -5, 7: -7.5, 8: -10, 9: -12.5, 10: -15, 11: -17.5, 12: -20,
}
const SLOWEST_SENSITIVITY_DB = -20
const SHOWN_UNDER_FLOOR_DB = 10

export interface Radio { freq?: number; bw?: number; sf?: number; power_dbm?: number }

export function noiseFloor(bwHz: number, noiseFigureDb = 6): number {
  return THERMAL_DBM_PER_HZ + 10 * Math.log10(Math.max(bwHz, 1)) + noiseFigureDb
}

export function threshold(bwHz: number, sf: number | undefined, noiseFigureDb = 6): number {
  return noiseFloor(bwHz, noiseFigureDb) + (sf ? SENSITIVITY_DB[sf] ?? SLOWEST_SENSITIVITY_DB : SLOWEST_SENSITIVITY_DB)
}

/**
 * The marks for `from`, over every other node in the table.
 * `gains` maps a node to its antenna gain; `heard`, when a running
 * simulation has answered a `levels` request, is the ether's own list of who
 * decodes and at what level, and overrides the computed figure for those.
 */
export function linksFrom(table: LossTable, from: string, gains: Record<string, number>,
                          radio: Radio, heard: Record<string, number> | null = null,
                          noiseFigureDb = 6): LinkMark[] {
  const f0 = Number(table.header.f0_hz) || 0
  const freq = radio.freq || f0
  const correction = f0 && freq ? 20 * Math.log10(freq / f0) : 0
  const bw = radio.bw || 125_000
  const power = radio.power_dbm ?? DEFAULT_POWER_DBM
  const decode = threshold(bw, radio.sf, noiseFigureDb)
  const floor = noiseFloor(bw, noiseFigureDb) - SHOWN_UNDER_FLOOR_DB
  const out: LinkMark[] = []
  for (const name of table.names) {
    if (name === from) continue
    const c = cell(table, from, name)
    if (!c) continue
    const los = (c.flags & FLAG_LOS_CLEAR) !== 0
    const said = heard?.[name]
    if (said !== undefined) { out.push({ name, level: said, decodable: true, los }); continue }
    if (!Number.isFinite(c.loss)) continue
    const level = power + (gains[from] ?? 0) + (gains[name] ?? 0) - c.loss - correction
    if (level < floor) continue
    out.push({ name, level, decodable: heard ? false : level >= decode, los })
  }
  return out
}
