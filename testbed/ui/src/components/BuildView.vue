<template>
  <div class="bv">
    <q-toolbar class="bv-bar">
      <q-btn flat dense no-caps label="‹ Back" @click="emit('back')" />
      <div class="bv-title">Build from sources</div>
      <q-space />
      <PlaceSearch nominatim @found="goToPlace" />
    </q-toolbar>
    <div class="bv-main">
      <div ref="wrap" class="bv-map">
        <canvas ref="canvas" :class="{ 'bv-banding': banding }"
                @pointerdown="onDown" @pointermove="onMove" @pointerup="onUp" @pointercancel="onUp"
                @wheel.prevent="onWheel" />
        <div class="bv-hint">drag to pan, wheel to zoom, Ctrl/Cmd-drag draws the rectangle</div>
        <div class="bv-credit">
          © <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap</a> contributors
        </div>
      </div>

      <div class="bv-side">
        <div v-if="!rect" class="bv-text">
          Draw the rectangle to build with Ctrl or Cmd and a drag on the map. Outlined
          rectangles are the packs there are; the tinted outlines are where the sources
          that do not cover the world apply: Berlin's own data, and Germany's census grid.
          The build takes the best source for each part of the rectangle.
        </div>
        <template v-else>
          <div class="bv-mono">{{ rect[1].toFixed(4) }}…{{ rect[3].toFixed(4) }} N,
            {{ rect[0].toFixed(4) }}…{{ rect[2].toFixed(4) }} E</div>
          <q-input v-model="name" dense outlined label="name"
                   hint="lower-case letters, digits and hyphens" />
          <div class="bv-row">
            <span class="bv-label">resolution</span>
            <q-btn-toggle v-model="res" dense no-caps unelevated toggle-color="primary"
                          :options="[{ label: '30 m', value: 30 }, { label: '10 m', value: 10 }]" />
          </div>
          <div v-if="plan" class="bv-text">
            {{ plan.grid.cells[0] }} × {{ plan.grid.cells[1] }} cells
            ({{ plan.grid.size_km[0] }} × {{ plan.grid.size_km[1] }} km),
            UTM zone {{ plan.grid.zone }} (EPSG:{{ plan.grid.epsg }});
            OpenStreetMap from Geofabrik's {{ plan.extract.name ?? plan.extract.id }} extract.
          </div>
          <div v-if="refused" class="bv-bad">{{ refused }}</div>

          <table v-if="plan" class="bv-table">
            <thead><tr><th>source</th><th>to fetch</th><th>licence</th></tr></thead>
            <tbody>
              <tr v-for="s in plan.sources" :key="s.source">
                <td>{{ s.title }}<div class="bv-dim">{{ s.used_for }}</div>
                  <div class="bv-dim">{{ s.files }} file{{ s.files === 1 ? '' : 's' }}</div></td>
                <td class="bv-mono">{{ fetchText(s) }}</td>
                <td class="bv-dim">{{ s.licence }}</td>
              </tr>
            </tbody>
          </table>
          <div v-else-if="asking" class="bv-dim">asking what it takes…</div>

          <q-btn unelevated no-caps color="primary" label="Build" :loading="starting"
                 :disable="!plan || !!refused || !name.trim() || !!catalog.build && catalog.build.state !== 'failed'"
                 @click="build" />
          <div v-if="catalog.build && catalog.build.state !== 'failed'" class="bv-dim">
            {{ catalog.build.name }} is being built: one build at a time.
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
/* Building a pack from its sources: a map to draw the rectangle on, and the
 * side panel saying what it takes.
 *
 * The map is OpenStreetMap's standard tiles in Web Mercator, fetched through
 * the front's /osm/, which keeps them (the tile usage policy asks for that,
 * and one origin keeps it one cache), drawn on a canvas as GroundMap draws:
 * a drag pans, the wheel zooms about the cursor, Ctrl/Cmd and a drag draws
 * the rectangle. Over the tiles: the areas of the sources that do not cover
 * the world, tinted, and the packs there are, outlined with their names.
 *
 * The side panel asks the front's /api/geodata/sources what the rectangle
 * takes at its resolution, whenever either changes: the grid, the sources
 * the front chose for it and what each is used for, and each source's
 * download still to fetch (what is in the cache costs nothing). Build starts
 * it on the front, a dialog says which sources go into the pack and for
 * what, and the view goes back to the list, where the build's row shows how
 * it goes. */
import { onMounted, onUnmounted, ref, watch, nextTick } from 'vue'
import { useQuasar } from 'quasar'
import PlaceSearch, { type FoundPlace } from './PlaceSearch.vue'
import { useCatalog } from '../stores/catalog'
import { slug } from '../lib/zip'

interface SourceRow {
  source: string; title: string; licence: string; used_for: string
  files: number; cached: number; to_fetch: number | null
}
interface Plan {
  grid: { zone: number; epsg: number; cells: [number, number]; size_km: [number, number] }
  extract: { id: string; name?: string }
  sources: SourceRow[]
}
type Geometry = { type: string; coordinates: number[][][] | number[][][][] } | null

const emit = defineEmits<{ back: []; started: [name: string] }>()
const catalog = useCatalog()
const quasar = useQuasar()
const wrap = ref<HTMLDivElement>()
const canvas = ref<HTMLCanvasElement>()

const TILE = 256
const MIN_ZOOM = 2
const MAX_ZOOM = 19
const TILES_KEPT = 400
const ASK_AFTER_MS = 400
const VIEW_KEY = 'simesh.buildview'

/* ── the side panel ── */
const rect = ref<[number, number, number, number] | null>(null)
const name = ref('')
const res = ref(30)
const plan = ref<Plan | null>(null)
const refused = ref<string | null>(null)
const asking = ref(false)
const starting = ref(false)
let askTimer: ReturnType<typeof setTimeout> | null = null
let askCtrl: AbortController | null = null

function mb(bytes: number) {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(0.1, bytes / 1e6).toFixed(1)} MB`
}

function fetchText(s: SourceRow) {
  if (s.cached === s.files) return 'in the cache'
  if (s.to_fetch === null) return 'size unknown'
  return `${mb(s.to_fetch)}${s.cached ? ` (${s.cached} cached)` : ''}`
}

function query(): URLSearchParams {
  return new URLSearchParams({
    bbox: rect.value!.map(v => v.toFixed(6)).join(','), res_m: String(res.value),
  })
}

function askSoon() {
  if (askTimer) clearTimeout(askTimer)
  if (!rect.value) return
  askTimer = setTimeout(() => { void ask() }, ASK_AFTER_MS)
}

async function ask() {
  askCtrl?.abort()
  const ctrl = new AbortController()
  askCtrl = ctrl
  asking.value = true
  try {
    const r = await fetch(`/api/geodata/sources?${query().toString()}`, { signal: ctrl.signal })
    const body = await r.json() as Plan & { ok: boolean; error?: string }
    if (ctrl.signal.aborted) return
    if (!body.ok) { plan.value = null; refused.value = body.error ?? 'refused'; return }
    refused.value = null
    plan.value = body
  } catch (e) {
    if (!ctrl.signal.aborted) { plan.value = null; refused.value = (e as Error).message }
  } finally {
    if (askCtrl === ctrl) asking.value = false
  }
}

watch(res, askSoon)

function escapeHtml(s: string) {
  return s.replace(/[&<>"']/g, c => `&#${c.charCodeAt(0)};`)
}

/* What the build takes, and for which part of the rectangle. */
function tellChosen(pack: string, rows: SourceRow[]) {
  const items = rows.map(s => `<li><b>${escapeHtml(s.title)}</b>: ${escapeHtml(s.used_for)}</li>`).join('')
  quasar.dialog({
    title: `Building ${escapeHtml(pack)}`,
    message: `The best source is taken for each part of the rectangle. The pack is built from:<ul>${items}</ul>`,
    html: true,
  })
}

async function build() {
  if (!rect.value) return
  starting.value = true
  try {
    const pack = name.value.trim()
    const rows = plan.value?.sources ?? []
    const r = await fetch('/api/geodata/build', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: pack, bbox: rect.value, res_m: res.value }),
    })
    const body = await r.json() as { ok: boolean; error?: string; build?: unknown }
    if (!body.ok) { quasar.notify({ type: 'negative', message: body.error ?? 'refused', timeout: 8000 }); return }
    tellChosen(pack, rows)
    emit('started', pack)
  } finally {
    starting.value = false
  }
}

/* ── Web Mercator ── */
const view = ref({ lon: 10, lat: 51, zoom: 5 })
let size = { w: 1, h: 1, dpr: 1 }
let ctx: CanvasRenderingContext2D | null = null
const tiles = new Map<string, HTMLImageElement>()
let areas: { berlin: Geometry; germany: Geometry } = { berlin: null, germany: null }

function worldPx(lon: number, lat: number, zoom: number): [number, number] {
  const w = TILE * 2 ** zoom
  const s = Math.sin(Math.max(-85.05, Math.min(85.05, lat)) * Math.PI / 180)
  return [(lon + 180) / 360 * w, (0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)) * w]
}

function lonLat(px: number, py: number, zoom: number): [number, number] {
  const w = TILE * 2 ** zoom
  const lon = px / w * 360 - 180
  const n = Math.PI - 2 * Math.PI * py / w
  return [lon, Math.atan(Math.sinh(n)) * 180 / Math.PI]
}

function toScreen(lon: number, lat: number): [number, number] {
  const [cx, cy] = worldPx(view.value.lon, view.value.lat, view.value.zoom)
  const [x, y] = worldPx(lon, lat, view.value.zoom)
  return [x - cx + size.w / 2, y - cy + size.h / 2]
}

function fromScreen(sx: number, sy: number): [number, number] {
  const [cx, cy] = worldPx(view.value.lon, view.value.lat, view.value.zoom)
  return lonLat(cx + sx - size.w / 2, cy + sy - size.h / 2, view.value.zoom)
}

function tile(z: number, x: number, y: number): HTMLImageElement {
  const key = `${z}/${x}/${y}`
  let img = tiles.get(key)
  if (img) { tiles.delete(key); tiles.set(key, img); return img }
  img = new Image()
  img.onload = () => schedule()
  img.src = `/osm/${key}.png`
  tiles.set(key, img)
  if (tiles.size > TILES_KEPT) tiles.delete(tiles.keys().next().value!)
  return img
}

let frame = 0
function schedule() {
  if (!frame) frame = requestAnimationFrame(() => { frame = 0; draw() })
}

function drawTiles(c: CanvasRenderingContext2D) {
  const z = Math.max(0, Math.min(MAX_ZOOM, Math.round(view.value.zoom)))
  const scale = 2 ** (view.value.zoom - z)
  const [cx, cy] = worldPx(view.value.lon, view.value.lat, z)
  const left = cx - size.w / 2 / scale, top = cy - size.h / 2 / scale
  const n = 2 ** z
  const x0 = Math.floor(left / TILE), y0 = Math.max(0, Math.floor(top / TILE))
  const x1 = Math.floor((left + size.w / scale) / TILE), y1 = Math.min(n - 1, Math.floor((top + size.h / scale) / TILE))
  for (let ty = y0; ty <= y1; ty++) {
    for (let tx = x0; tx <= x1; tx++) {
      const img = tile(z, ((tx % n) + n) % n, ty)
      const sx = (tx * TILE - left) * scale, sy = (ty * TILE - top) * scale
      if (img.complete && img.naturalWidth) c.drawImage(img, sx, sy, TILE * scale + 0.5, TILE * scale + 0.5)
    }
  }
}

function tracePolygons(c: CanvasRenderingContext2D, g: Geometry) {
  if (!g) return
  const polys = (g.type === 'Polygon' ? [g.coordinates] : g.coordinates) as number[][][][]
  c.beginPath()
  for (const poly of polys) {
    for (const ring of poly) {
      ring.forEach(([lon, lat], i) => {
        const [x, y] = toScreen(lon!, lat!)
        if (i) c.lineTo(x, y); else c.moveTo(x, y)
      })
      c.closePath()
    }
  }
}

function box(c: CanvasRenderingContext2D, b: number[]) {
  const [x0, y0] = toScreen(b[0]!, b[3]!)
  const [x1, y1] = toScreen(b[2]!, b[1]!)
  c.beginPath()
  c.rect(x0, y0, x1 - x0, y1 - y0)
  return [x0, y0, x1, y1] as const
}

function draw() {
  const c = ctx
  if (!c) return
  c.fillStyle = '#d4d0c8'
  c.fillRect(0, 0, size.w, size.h)
  drawTiles(c)
  for (const [g, colour] of [[areas.germany, '59, 130, 246'], [areas.berlin, '217, 70, 239']] as const) {
    tracePolygons(c, g)
    c.fillStyle = `rgba(${colour}, 0.08)`
    c.fill('evenodd')
    c.strokeStyle = `rgba(${colour}, 0.8)`
    c.lineWidth = 1.5
    c.stroke()
  }
  c.font = '12px system-ui, sans-serif'
  for (const g of catalog.geodata) {
    if (g.kind !== 'pack' || !g.bbox) continue
    const [x0, y0] = box(c, g.bbox)
    c.strokeStyle = '#1f2937'
    c.lineWidth = 1.5
    c.setLineDash([6, 4])
    c.stroke()
    c.setLineDash([])
    c.fillStyle = '#1f2937'
    c.fillText(g.name, x0 + 4, y0 + 14)
  }
  const r = band ? bandRect() : rect.value
  if (r) {
    box(c, r)
    c.fillStyle = 'rgba(234, 88, 12, 0.15)'
    c.fill()
    c.strokeStyle = '#ea580c'
    c.lineWidth = 2
    c.stroke()
  }
}

/* ── the pointer ── */
let drag: { x: number; y: number; lon: number; lat: number } | null = null
let band: { x0: number; y0: number; x1: number; y1: number } | null = null
const banding = ref(false)

function pointer(e: PointerEvent): [number, number] {
  const r = canvas.value!.getBoundingClientRect()
  return [e.clientX - r.left, e.clientY - r.top]
}

function bandRect(): [number, number, number, number] | null {
  if (!band) return null
  const [lonA, latA] = fromScreen(band.x0, band.y0)
  const [lonB, latB] = fromScreen(band.x1, band.y1)
  return [Math.min(lonA, lonB), Math.min(latA, latB), Math.max(lonA, lonB), Math.max(latA, latB)]
}

function onDown(e: PointerEvent) {
  const [x, y] = pointer(e)
  canvas.value!.setPointerCapture(e.pointerId)
  if (e.ctrlKey || e.metaKey) {
    band = { x0: x, y0: y, x1: x, y1: y }
    banding.value = true
  } else {
    drag = { x, y, lon: view.value.lon, lat: view.value.lat }
  }
}

function onMove(e: PointerEvent) {
  const [x, y] = pointer(e)
  if (band) { band.x1 = x; band.y1 = y; schedule(); return }
  if (!drag) return
  const [cx, cy] = worldPx(drag.lon, drag.lat, view.value.zoom)
  const [lon, lat] = lonLat(cx - (x - drag.x), cy - (y - drag.y), view.value.zoom)
  view.value = { ...view.value, lon, lat }
  schedule()
}

function onUp() {
  if (band) {
    const r = bandRect()
    if (r && Math.abs(band.x1 - band.x0) > 4 && Math.abs(band.y1 - band.y0) > 4) {
      rect.value = r
      plan.value = null
      askSoon()
    }
    band = null
    banding.value = false
    schedule()
  }
  if (drag) saveView()
  drag = null
}

function onWheel(e: WheelEvent) {
  const [x, y] = [e.offsetX, e.offsetY]
  const [lon, lat] = fromScreen(x, y)
  const zoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, view.value.zoom - e.deltaY * 0.002))
  // Keep the point under the cursor where it is.
  const [px, py] = worldPx(lon, lat, zoom)
  const [clon, clat] = lonLat(px - (x - size.w / 2), py - (y - size.h / 2), zoom)
  view.value = { lon: clon, lat: clat, zoom }
  schedule()
  saveView()
}

function fitBox(b: number[]) {
  const [x0, y0] = worldPx(b[0]!, b[3]!, 0)
  const [x1, y1] = worldPx(b[2]!, b[1]!, 0)
  const zoom = Math.log2(Math.min(size.w / Math.max(1e-9, x1 - x0), size.h / Math.max(1e-9, y1 - y0)) * 0.8)
  const [lon, lat] = lonLat((x0 + x1) / 2, (y0 + y1) / 2, 0)
  view.value = { lon, lat, zoom: Math.max(MIN_ZOOM, Math.min(16, zoom)) }
  schedule()
  saveView()
}

function goToPlace(p: FoundPlace) {
  fitBox(p.bbox)
  if (!name.value) name.value = slug(p.name.split(',')[0] ?? '')
}

function saveView() {
  try { localStorage.setItem(VIEW_KEY, JSON.stringify(view.value)) } catch { /* private window */ }
}

function restoreView() {
  try {
    const got = JSON.parse(localStorage.getItem(VIEW_KEY) ?? 'null') as typeof view.value | null
    if (got && Number.isFinite(got.zoom)) view.value = got
  } catch { /* nothing kept */ }
}

function resize() {
  const element = wrap.value, surface = canvas.value
  if (!element || !surface) return
  const dpr = window.devicePixelRatio || 1
  size = { w: element.clientWidth, h: element.clientHeight, dpr }
  surface.width = Math.round(size.w * dpr)
  surface.height = Math.round(size.h * dpr)
  surface.style.width = `${size.w}px`
  surface.style.height = `${size.h}px`
  ctx = surface.getContext('2d')
  ctx?.setTransform(dpr, 0, 0, dpr, 0, 0)
  schedule()
}

let observer: ResizeObserver | null = null
onMounted(async () => {
  await nextTick()
  restoreView()
  resize()
  observer = new ResizeObserver(resize)
  if (wrap.value) observer.observe(wrap.value)
  try {
    const r = await fetch('/api/geodata/areas')
    const body = await r.json() as { ok: boolean; berlin?: Geometry; germany?: Geometry }
    if (body.ok) { areas = { berlin: body.berlin ?? null, germany: body.germany ?? null }; schedule() }
  } catch { /* the outlines are a help, not a need */ }
})
onUnmounted(() => {
  observer?.disconnect()
  if (frame) cancelAnimationFrame(frame)
  if (askTimer) clearTimeout(askTimer)
  askCtrl?.abort()
})
watch(() => catalog.geodata, schedule)
</script>

<style scoped>
.bv { display: flex; flex-direction: column; height: 100%; }
.bv-bar { min-height: 38px; gap: 6px; padding-left: 4px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.bv-title { font-size: 14px; font-weight: 500; padding: 0 10px; }
.bv-main { flex: 1 1 auto; min-height: 0; display: flex; }
.bv-map { position: relative; flex: 1 1 auto; min-width: 0; overflow: hidden; }
.bv-map canvas { display: block; cursor: grab; touch-action: none; }
.bv-map canvas:active { cursor: grabbing; }
.bv-map canvas.bv-banding { cursor: crosshair; }
.bv-hint {
  position: absolute; left: 10px; bottom: 8px; font-size: 11px; color: #e5e7eb;
  background: rgba(18, 20, 23, 0.75); padding: 2px 6px; border-radius: 3px; pointer-events: none;
}
.bv-credit {
  position: absolute; right: 0; bottom: 0; font-size: 11px; color: #111827;
  background: rgba(255, 255, 255, 0.8); padding: 1px 6px;
}
.bv-credit a { color: #1d4ed8; }
.bv-side {
  width: 340px; flex: none; overflow-y: auto; padding: 12px 14px 24px; display: flex;
  flex-direction: column; gap: 8px; border-left: 1px solid #262c35; background: #15181d;
}
.bv-text { font-size: 12px; color: #9ca3af; line-height: 1.5; }
.bv-label { font-size: 11px; color: #6b7280; margin-top: 4px; }
.bv-row { display: flex; align-items: center; gap: 10px; }
.bv-bad { font-size: 12px; color: #fca5a5; }
.bv-dim { font-size: 11px; color: #6b7280; }
.bv-mono { font-family: ui-monospace, monospace; font-size: 12px; }
.bv-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.bv-table th { text-align: left; font-weight: 500; font-size: 11px; color: #6b7280; padding: 3px 4px; border-bottom: 1px solid #262c35; }
.bv-table td { padding: 4px; border-bottom: 1px solid #1f242c; vertical-align: top; }
</style>
