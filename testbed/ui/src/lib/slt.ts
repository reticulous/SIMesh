/* The loss table file, read: every ordered pair's path loss for one nodeset
 * on one geodata, as ether/slt.py writes it.
 *
 *     "SLT1" | u32 header_len | header (UTF-8 JSON) | u32 n
 *            | f32 loss_db[n·n]     row-major, [from][to]; the diagonal unused
 *            | u8  flags[n·n]
 *            | u16 samples[n·n]
 *
 * Little-endian throughout. "Never heard" is +inf. */

export const FLAG_NEAR_FIELD = 1 << 0
export const FLAG_OFF_PACK = 1 << 1
export const FLAG_BEYOND_RADIUS = 1 << 2
export const FLAG_LOS_CLEAR = 1 << 3
export const FLAG_MEASURED = 1 << 4

export interface TableHeader {
  geodata: string
  band: string
  f0_hz: number
  model: string
  nodes: { name: string; id: number; lat: number; lon: number; height_m: number; height_from: string }[]
  [key: string]: unknown
}

export interface LossTable {
  header: TableHeader
  names: string[]
  index: Record<string, number>
  loss: Float32Array
  flags: Uint8Array
}

export function readTable(buf: ArrayBuffer): LossTable {
  const dv = new DataView(buf)
  const magic = String.fromCharCode(dv.getUint8(0), dv.getUint8(1), dv.getUint8(2), dv.getUint8(3))
  if (magic !== 'SLT1') throw new Error('not a loss table')
  const headLen = dv.getUint32(4, true)
  const header = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 8, headLen))) as TableHeader
  let at = 8 + headLen
  const n = dv.getUint32(at, true)
  at += 4
  /* Copied out: the f32 block need not sit on a 4-byte boundary. */
  const loss = new Float32Array(buf.slice(at, at + n * n * 4))
  at += n * n * 4
  const flags = new Uint8Array(buf.slice(at, at + n * n))
  const names = (header.nodes ?? []).map(d => d.name)
  const index: Record<string, number> = {}
  names.forEach((name, i) => { index[name] = i })
  return { header, names, index, loss, flags }
}

/** The loss and flags from `a` to `b`, or null when either is not in the table. */
export function cell(t: LossTable, a: string, b: string): { loss: number; flags: number } | null {
  const i = t.index[a], j = t.index[b]
  if (i === undefined || j === undefined || i === j) return null
  const k = i * t.names.length + j
  return { loss: t.loss[k]!, flags: t.flags[k]! }
}
