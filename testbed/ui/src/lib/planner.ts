/* The planner's sidecar, as the page uses it.
 *
 * The front runs one planner-web per pack in use and passes `/planner/<geodata>/…`
 * through to it with the prefix stripped, so every path below is planner-web's
 * own route under that prefix. Coordinates are the pack's CRS metres
 * throughout. The binary formats are planner-web's, read here as its own page
 * reads them:
 *
 *     tile.bin       "PTL2" | u32 w | u32 h | f64 ox | f64 oy | f64 rx | f64 ry | u8 flags
 *                    | i16 terrain[w·h] (dm, −32768 none) | [u8 classes] (flags&1)
 *                    | [u16 population·10] (flags&2) | [u16 clutter height·10] (flags&4)
 *     basemap.bin    "PBM1" | u32 w | u32 h | … | rgba[w·h] from byte 44
 *     buildings.bin  "PBO3" | u32 rings | u8 truncated | f64 ox | f64 oy
 *                    | per ring: u32 id, u32 n, f32 top (m above sea level), f32 loss,
 *                      f32 census, n × (f32 dx, f32 dy) from (ox, oy)
 *     roads.bin      per way: f32 class, f32 n, n × (f32 x, f32 y), no header
 *
 * The base map is planner-wasm's: a Tile built from tile.bin's terrain, given
 * the server-baked base image, and composed; the page draws the result with
 * drawImage, so panning and zooming cost nothing until the view leaves it.
 * Footprints and roads are the page's own to draw. */
import init, { Tile } from 'planner-wasm'
import wasmUrl from 'planner-wasm/planner_wasm_bg.wasm?url'

/** The ground under everything. Population is not one: it is a heatmap of
 *  its own over whichever ground (`populationImage`). */
export type BaseLayer = 'terrain' | 'clutter'
/** planner-wasm's layer numbers. */
const LAYER: Record<BaseLayer, number> = { terrain: 0, clutter: 2 }

export interface Box { minx: number; miny: number; maxx: number; maxy: number }

export interface PackInfo {
  name: string
  crs_epsg: number
  extent: Box
  centre: { lat: number; lon: number }
  res_m: number
  layers: string[]
  licenses: { source: string; notice: string }[]
}

export function plannerBase(geodata: string): string {
  return `/planner/${encodeURIComponent(geodata)}`
}

let ready: Promise<unknown> | null = null
/** planner-wasm, instantiated once for the page. */
export function wasm(): Promise<unknown> {
  if (!ready) ready = init({ module_or_path: wasmUrl })
  return ready
}

/** GET, backing off while the sidecar sheds load with 429. */
export async function getWithBackoff(url: string, signal?: AbortSignal): Promise<Response> {
  for (let attempt = 0; ; attempt++) {
    const r = await fetch(url, { signal })
    if (r.status !== 429 || attempt >= 6) return r
    await new Promise(res => setTimeout(res, 150 * (attempt + 1)))
    if (signal?.aborted) throw new DOMException('aborted', 'AbortError')
  }
}

function boxQuery(b: Box, w: number, h: number, extra: Record<string, string | number> = {}) {
  const q = new URLSearchParams()
  for (const [k, v] of Object.entries({ ...b, w, h, ...extra })) q.set(k, String(v))
  return q.toString()
}

function magic(dv: DataView): string {
  return String.fromCharCode(dv.getUint8(0), dv.getUint8(1), dv.getUint8(2), dv.getUint8(3))
}

export async function packInfo(base: string, signal?: AbortSignal): Promise<PackInfo> {
  const r = await fetch(`${base}/api/pack`, { signal })
  if (!r.ok) throw new Error(`the planner sidecar answered ${r.status}: ${(await r.text()).slice(0, 200)}`)
  return await r.json() as PackInfo
}

interface DecodedTile {
  w: number; h: number
  ox: number; oy: number; rx: number; ry: number
  terrain: Float32Array
  /** Residents per cell, when the tile carries them. */
  population: Float32Array | null
  clutter: Float32Array | null
}

function decodeTile(buf: ArrayBuffer): DecodedTile {
  const dv = new DataView(buf)
  if (magic(dv) !== 'PTL2') throw new Error('not a tile')
  const w = dv.getUint32(4, true), h = dv.getUint32(8, true)
  const ox = dv.getFloat64(12, true), oy = dv.getFloat64(20, true)
  const rx = dv.getFloat64(28, true), ry = dv.getFloat64(36, true)
  const flags = dv.getUint8(44)
  const n = w * h
  let off = 45
  const raw = new Int16Array(buf.slice(off, off + n * 2))
  const terrain = new Float32Array(n)
  for (let i = 0; i < n; i++) { const v = raw[i]!; terrain[i] = v === -32768 ? NaN : v * 0.1 }
  off += n * 2
  if (flags & 1) off += n
  let population: Float32Array | null = null
  if (flags & 2) {
    const u = new Uint16Array(buf.slice(off, off + n * 2))
    population = new Float32Array(n)
    for (let i = 0; i < n; i++) population[i] = u[i]! * 0.1
    off += n * 2
  }
  let clutter: Float32Array | null = null
  if (flags & 4) {
    const u = new Uint16Array(buf.slice(off, off + n * 2))
    clutter = new Float32Array(n)
    for (let i = 0; i < n; i++) clutter[i] = u[i]! * 0.1
  }
  return { w, h, ox, oy, rx, ry, terrain, population, clutter }
}

/** One heatmap image and the ground rectangle it covers. */
export interface Heatmap { canvas: HTMLCanvasElement; bounds: Box }

/* Residents per cell of the pack's population raster, log-scaled as the
 * planner's own map scales them (density spans orders of magnitude): a
 * trace is a faint violet, a dense block a bright orange-yellow. */
function populationColour(v: number): [number, number, number, number] | null {
  if (!(v > 0.02)) return null
  const f = Math.min(1, Math.log(v + 1) / Math.log(6))
  const r = Math.round(120 + f * 135), g = Math.round(40 + f * 170), b = Math.round(170 - f * 130)
  return [r, g, b, Math.round(70 + f * 150)]
}

/** The population in `box` at `w`×`h` cells, as a heatmap; null when the
 *  pack has no population layer. */
export async function populationImage(base: string, box: Box, w: number, h: number,
                                      signal?: AbortSignal): Promise<Heatmap | null> {
  const r = await getWithBackoff(`${base}/tile.bin?${boxQuery(box, w, h)}`, signal)
  if (!r.ok) throw new Error(`tile ${r.status}`)
  const d = decodeTile(await r.arrayBuffer())
  if (!d.population) return null
  const canvas = document.createElement('canvas')
  canvas.width = d.w
  canvas.height = d.h
  const c = canvas.getContext('2d')!
  const img = c.createImageData(d.w, d.h)
  for (let i = 0; i < d.w * d.h; i++) {
    const colour = populationColour(d.population[i]!)
    if (!colour) continue
    img.data.set(colour, i * 4)
  }
  c.putImageData(img, 0, 0)
  // The origin is the top-left cell's centre: the image covers half a cell more.
  return {
    canvas,
    bounds: { minx: d.ox - d.rx / 2, maxx: d.ox + (d.w - 0.5) * d.rx,
              maxy: d.oy + d.ry / 2, miny: d.oy - (d.h - 0.5) * d.ry },
  }
}

/** A copy of an image in grey, for what lies under a heatmap. */
export function greyed(src: HTMLCanvasElement): HTMLCanvasElement {
  const out = document.createElement('canvas')
  out.width = src.width
  out.height = src.height
  if (!src.width || !src.height) return out
  const c = out.getContext('2d')!
  c.drawImage(src, 0, 0)
  const img = c.getImageData(0, 0, out.width, out.height)
  const p = img.data
  for (let i = 0; i < p.length; i += 4) {
    const l = Math.round(0.299 * p[i]! + 0.587 * p[i + 1]! + 0.114 * p[i + 2]!)
    p[i] = l; p[i + 1] = l; p[i + 2] = l
  }
  c.putImageData(img, 0, 0)
  return out
}

/** Terrain heights on a grid: the origin is the top-left cell's centre, rows run south. */
export interface Grid { w: number; h: number; ox: number; oy: number; rx: number; ry: number; terrain: Float32Array }

/** One composed base image, the ground rectangle it covers, and the terrain under it. */
export interface BaseImage {
  canvas: HTMLCanvasElement
  bounds: Box
  grid: Grid
}

/**
 * The base map for `box` at `w`×`h` cells: tile.bin for the grid, the
 * server's bake of `layer` for the pixels, planner-wasm to compose them (and
 * to bake them itself, from the terrain, when the server's image does not
 * come). Roads are not burnt in: baked at the pack's cell size they are tens
 * of metres wide close up, so the map draws them from roads.bin as lines.
 * The Tile is freed as soon as the image is out of it: its memory is WASM
 * linear memory, which never shrinks, and the page keeps only the image.
 */
export async function baseImage(base: string, box: Box, w: number, h: number,
                                layer: BaseLayer, signal?: AbortSignal): Promise<BaseImage> {
  await wasm()
  const q = boxQuery(box, w, h, { terrain_only: 1 })
  const [tr, br] = await Promise.all([
    getWithBackoff(`${base}/tile.bin?${q}`, signal),
    getWithBackoff(`${base}/basemap.bin?${q}&layer=${LAYER[layer]}&roads=0`, signal)
      .catch((e: unknown) => { if ((e as Error).name === 'AbortError') throw e; return null }),
  ])
  if (!tr.ok) throw new Error(`tile ${tr.status}`)
  const d = decodeTile(await tr.arrayBuffer())
  const tile = new Tile(d.ox, d.oy, d.rx, d.ry, d.w, d.h, d.terrain)
  try {
    if (br && br.ok) {
      const buf = await br.arrayBuffer()
      const dv = new DataView(buf)
      if (magic(dv) === 'PBM1') {
        const bw = dv.getUint32(4, true), bh = dv.getUint32(8, true)
        tile.set_baked(bw, bh, new Uint8Array(buf, 44, bw * bh * 4))
      }
    }
    const rgba = tile.compose_base(LAYER[layer], false, false)
    const [minx, miny, maxx, maxy] = tile.outer_bounds() as unknown as number[]
    const canvas = document.createElement('canvas')
    canvas.width = d.w
    canvas.height = d.h
    canvas.getContext('2d')!.putImageData(
      new ImageData(new Uint8ClampedArray(rgba.buffer as ArrayBuffer, rgba.byteOffset, rgba.length), d.w, d.h), 0, 0)
    return {
      canvas,
      bounds: { minx: minx!, miny: miny!, maxx: maxx!, maxy: maxy! },
      grid: { w: d.w, h: d.h, ox: d.ox, oy: d.oy, rx: d.rx, ry: d.ry, terrain: d.terrain },
    }
  } finally {
    tile.free()
  }
}

/** A road or railway: its class (0 motorway, 1 trunk, 2 primary, 3 secondary,
 *  4 rail) and its points in absolute metres. */
export interface Way { cls: number; pts: Float32Array; box: Box }

/** The ways in `box`. The sidecar keeps whole ways in the pack's order up to
 *  a byte budget, so a view of a whole city gets its first share of them. */
export async function roads(base: string, box: Box, signal?: AbortSignal): Promise<Way[]> {
  const r = await getWithBackoff(`${base}/roads.bin?${boxQuery(box, 1, 1)}`, signal)
  if (!r.ok) throw new Error(`roads ${r.status}`)
  const all = new Float32Array(await r.arrayBuffer())
  const ways: Way[] = []
  for (let at = 0; at + 2 <= all.length;) {
    const cls = all[at]!, n = all[at + 1]!
    const pts = all.subarray(at + 2, at + 2 + n * 2)
    at += 2 + n * 2
    const b = { minx: Infinity, miny: Infinity, maxx: -Infinity, maxy: -Infinity }
    for (let k = 0; k < n; k++) {
      const x = pts[k * 2]!, y = pts[k * 2 + 1]!
      if (x < b.minx) b.minx = x
      if (x > b.maxx) b.maxx = x
      if (y < b.miny) b.miny = y
      if (y > b.maxy) b.maxy = y
    }
    ways.push({ cls, pts, box: b })
  }
  return ways
}

/** Ground and clutter height at one point, from the pack's rasters. */
export async function sample(base: string, x: number, y: number,
                             signal?: AbortSignal): Promise<{ ground: number; clutter: number | null }> {
  const r = 2
  const q = boxQuery({ minx: x - r, miny: y - r, maxx: x + r, maxy: y + r }, 1, 1)
  const res = await getWithBackoff(`${base}/tile.bin?${q}`, signal)
  if (!res.ok) throw new Error(`tile ${res.status}`)
  const d = decodeTile(await res.arrayBuffer())
  /* The origin is the top-left cell's centre, rows running south. */
  const col = Math.min(d.w - 1, Math.max(0, Math.round((x - d.ox) / Math.abs(d.rx))))
  const row = Math.min(d.h - 1, Math.max(0, Math.round((d.oy - y) / Math.abs(d.ry))))
  const k = row * d.w + col
  return { ground: d.terrain[k]!, clutter: d.clutter ? d.clutter[k]! : null }
}

/** One building's footprint: its rings in absolute metres, its roof above sea level. */
export interface Footprint {
  id: number
  rings: Float64Array[]
  top: number
  box: Box
}

export interface Footprints {
  truncated: boolean
  list: Footprint[]
}

/** The footprints in `box`, or null when the pack has no building geometry
 *  (a pack built without `--lod2-geometry` answers 204). */
export async function buildings(base: string, box: Box, signal?: AbortSignal): Promise<Footprints | null> {
  const r = await getWithBackoff(`${base}/buildings.bin?${boxQuery(box, 1, 1)}`, signal)
  if (r.status === 204) return null
  if (!r.ok) throw new Error(`buildings ${r.status}`)
  const buf = await r.arrayBuffer()
  const dv = new DataView(buf)
  if (magic(dv) !== 'PBO3') throw new Error('not a footprint reply')
  const count = dv.getUint32(4, true)
  const truncated = !!dv.getUint8(8)
  const ox = dv.getFloat64(9, true), oy = dv.getFloat64(17, true)
  let off = 25
  const list: Footprint[] = []
  let cur: Footprint | null = null
  for (let i = 0; i < count; i++) {
    const id = dv.getUint32(off, true)
    const n = dv.getUint32(off + 4, true)
    const top = dv.getFloat32(off + 8, true)
    off += 20
    const rel = new Float32Array(buf.slice(off, off + n * 8))
    off += n * 8
    if (!cur || cur.id !== id) {
      cur = { id, rings: [], top, box: { minx: Infinity, miny: Infinity, maxx: -Infinity, maxy: -Infinity } }
      list.push(cur)
    }
    const ring = new Float64Array(n * 2)
    for (let k = 0; k < n; k++) {
      const px = ox + rel[k * 2]!, py = oy + rel[k * 2 + 1]!
      ring[k * 2] = px
      ring[k * 2 + 1] = py
      if (px < cur.box.minx) cur.box.minx = px
      if (px > cur.box.maxx) cur.box.maxx = px
      if (py < cur.box.miny) cur.box.miny = py
      if (py > cur.box.maxy) cur.box.maxy = py
    }
    cur.rings.push(ring)
  }
  return { truncated, list }
}

/** Whether (x, y) is inside a footprint: inside its outer ring and not in a hole. */
export function inside(f: Footprint, x: number, y: number): boolean {
  if (x < f.box.minx || x > f.box.maxx || y < f.box.miny || y > f.box.maxy) return false
  let hit = false
  for (const ring of f.rings) {
    const n = ring.length / 2
    for (let i = 0, j = n - 1; i < n; j = i++) {
      const xi = ring[i * 2]!, yi = ring[i * 2 + 1]!, xj = ring[j * 2]!, yj = ring[j * 2 + 1]!
      if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) hit = !hit
    }
  }
  return hit
}

export interface Place {
  kind: string
  name: string
  ctx: string
  x: number
  y: number
  lat: number
  lon: number
}

export async function search(base: string, text: string, signal?: AbortSignal): Promise<Place[]> {
  const r = await fetch(`${base}/search?q=${encodeURIComponent(text)}`, { signal })
  if (!r.ok) return []
  return ((await r.json()) as { hits: Place[] }).hits ?? []
}

/** One point of link.json's profile: distance in km, and heights in metres
 *  above sea level of the terrain (`h`), the surface with clutter and
 *  buildings (`g`), the ray, and the first Fresnel radius there. */
export interface ProfilePoint { d: number; h: number; g: number; e: number; ray: number; f1: number; clr: number }

export interface LinkReply {
  lb_db: number
  distance_km: number
  /** The antennas' heights above the ground, where they are. */
  tx_h: number
  rx_h: number
  /** An end inside a building: what its walls cost, dB; null in the open. */
  tx_indoor_entry_db?: number | null
  rx_indoor_entry_db?: number | null
  fresnel: { verdict: 'clear' | 'grazing' | 'obstructed'; worst_clearance_m: number; worst_d_km: number }
  profile_evidence?: { model?: string; [k: string]: unknown }
  terminal_clutter?: unknown
  profile: ProfilePoint[]
  [key: string]: unknown
}

/** link.json between two points, or the sidecar's refusal as an Error. */
export async function link(base: string, a: [number, number], b: [number, number],
                           txH: number, rxH: number, txGain?: number, rxGain?: number,
                           signal?: AbortSignal): Promise<LinkReply> {
  const q = new URLSearchParams({
    ax: String(a[0]), ay: String(a[1]), bx: String(b[0]), by: String(b[1]),
    tx_h: String(txH), rx_h: String(rxH),
  })
  if (txGain !== undefined) q.set('tx_gain_dbi', String(txGain))
  if (rxGain !== undefined) q.set('rx_gain_dbi', String(rxGain))
  const r = await getWithBackoff(`${base}/link.json?${q}`, signal)
  if (!r.ok) throw new Error((await r.text()).trim() || `link.json ${r.status}`)
  return await r.json() as LinkReply
}
