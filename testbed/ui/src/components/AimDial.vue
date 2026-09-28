<template>
  <div class="aim">
    <figure class="aim-fig">
      <svg viewBox="-60 -60 120 120" class="aim-svg" tabindex="0" role="slider"
           aria-label="azimuth" :aria-valuenow="shownAz"
           @pointerdown="startDrag($event, 'az')" @wheel.prevent="nudge('az', $event.deltaY < 0 ? 1 : -1, $event.shiftKey)"
           @keydown.left.prevent="nudge('az', -1, $event.shiftKey)" @keydown.right.prevent="nudge('az', 1, $event.shiftKey)">
        <circle r="50" class="aim-face" />
        <line v-for="t in ticks" :key="t.deg" :x1="t.x1" :y1="t.y1" :x2="t.x2" :y2="t.y2"
              :class="t.major ? 'aim-major' : 'aim-minor'" />
        <text v-for="c in compass" :key="c.t" :x="c.x" :y="c.y" class="aim-letter">{{ c.t }}</text>
        <path :d="wedge(azRad, hbw, 46)" class="aim-beam" />
        <line x1="0" y1="0" :x2="46 * Math.sin(azRad)" :y2="-46 * Math.cos(azRad)" class="aim-needle" />
        <circle r="3" class="aim-hub" />
      </svg>
      <figcaption>
        azimuth <b>{{ shownAz }}°</b><span v-if="mixed.az" class="aim-mixed"> (differs)</span>
      </figcaption>
    </figure>

    <figure class="aim-fig">
      <svg viewBox="-12 -60 72 120" class="aim-svg aim-side" tabindex="0" role="slider"
           aria-label="elevation" :aria-valuenow="shownEl"
           @pointerdown="startDrag($event, 'el')" @wheel.prevent="nudge('el', $event.deltaY < 0 ? 1 : -1, $event.shiftKey)"
           @keydown.up.prevent="nudge('el', 1, $event.shiftKey)" @keydown.down.prevent="nudge('el', -1, $event.shiftKey)">
        <path d="M 0 -50 A 50 50 0 0 1 0 50" class="aim-face" />
        <line x1="0" y1="0" x2="54" y2="0" class="aim-horizon" />
        <line v-for="t in sideTicks" :key="t.deg" :x1="t.x1" :y1="t.y1" :x2="t.x2" :y2="t.y2"
              :class="t.major ? 'aim-major' : 'aim-minor'" />
        <text x="53" y="-3" class="aim-letter aim-small">0°</text>
        <text x="-2" y="-52" class="aim-letter aim-small">+90</text>
        <text x="-2" y="58" class="aim-letter aim-small">−90</text>
        <line x1="-6" y1="0" x2="-6" y2="56" class="aim-mast" />
        <path :d="wedge(Math.PI / 2 - elRad, vbw, 46)" class="aim-beam" />
        <line x1="0" y1="0" :x2="46 * Math.cos(elRad)" :y2="-46 * Math.sin(elRad)" class="aim-needle" />
        <circle r="3" class="aim-hub" />
      </svg>
      <figcaption>
        elevation <b>{{ shownEl > 0 ? '+' : '' }}{{ shownEl }}°</b><span v-if="mixed.el" class="aim-mixed"> (differs)</span>
      </figcaption>
    </figure>
    <div class="aim-hint">Drag, scroll or use the arrow keys; Shift for 5°.</div>
  </div>
</template>

<script setup lang="ts">
/* A directional antenna's aim, drawn: a compass for its azimuth (clockwise
 * from north) and a side view for its elevation (above the horizon), each
 * with the beam's half-power width as a wedge. Dragging, scrolling or the
 * arrow keys change it; while a drag goes on the new aim is sent a few times
 * a second, so the coverage follows the needle, and once more where it ends. */
import { computed, onBeforeUnmount, ref } from 'vue'

const props = defineProps<{
  azimuth: number
  elevation: number
  /** The beam's half-power widths, for the wedges. */
  hbw: number
  vbw: number
  /** The selected nodes do not share this figure. */
  mixed: { az: boolean; el: boolean }
}>()
const emit = defineEmits<{ aim: [change: { azimuth_deg?: number; elevation_deg?: number }] }>()

const SEND_MS = 200

/* What the dial shows: the figure being dragged to, else the node's. */
const dragging = ref<{ az?: number; el?: number }>({})
const shownAz = computed(() => Math.round(dragging.value.az ?? props.azimuth) % 360)
const shownEl = computed(() => Math.round(dragging.value.el ?? props.elevation))
const azRad = computed(() => shownAz.value * Math.PI / 180)
const elRad = computed(() => shownEl.value * Math.PI / 180)

const ticks = Array.from({ length: 36 }, (_, i) => {
  const a = i * 10 * Math.PI / 180, major = i % 9 === 0
  const r0 = major ? 42 : 46
  return { deg: i * 10, major, x1: r0 * Math.sin(a), y1: -r0 * Math.cos(a), x2: 50 * Math.sin(a), y2: -50 * Math.cos(a) }
})
const compass = [{ t: 'N', x: -3.5, y: -34 }, { t: 'E', x: 32, y: 4 }, { t: 'S', x: -3.5, y: 40 }, { t: 'W', x: -40, y: 4 }]
const sideTicks = Array.from({ length: 19 }, (_, i) => {
  const deg = -90 + i * 10, a = deg * Math.PI / 180, major = deg % 45 === 0
  const r0 = major ? 42 : 46
  return { deg, major, x1: r0 * Math.cos(a), y1: -r0 * Math.sin(a), x2: 50 * Math.cos(a), y2: -50 * Math.sin(a) }
})

/** A pie slice of `width` degrees centred on the compass angle `centre` (radians, clockwise from up). */
function wedge(centre: number, width: number, r: number) {
  const half = Math.min(width, 358) / 2 * Math.PI / 180
  const a0 = centre - half, a1 = centre + half
  const large = half * 2 > Math.PI ? 1 : 0
  return `M 0 0 L ${r * Math.sin(a0)} ${-r * Math.cos(a0)} A ${r} ${r} 0 ${large} 1 ${r * Math.sin(a1)} ${-r * Math.cos(a1)} Z`
}

/* ── dragging ── */
let which: 'az' | 'el' | null = null
let svg: SVGSVGElement | null = null
let timer: ReturnType<typeof setTimeout> | null = null
let dirty = false

function valueAt(e: PointerEvent): number | null {
  if (!svg) return null
  const box = svg.getBoundingClientRect()
  const vb = svg.viewBox.baseVal
  const x = vb.x + (e.clientX - box.left) / box.width * vb.width
  const y = vb.y + (e.clientY - box.top) / box.height * vb.height
  if (Math.hypot(x, y) < 4) return null
  if (which === 'az') return ((Math.atan2(x, -y) * 180 / Math.PI) + 360) % 360
  return Math.max(-90, Math.min(90, Math.atan2(-y, Math.max(x, 0.001)) * 180 / Math.PI))
}

function take(v: number | null) {
  if (v === null || !which) return
  dragging.value = { ...dragging.value, [which]: Math.round(v) }
  dirty = true
  if (!timer) timer = setTimeout(flush, SEND_MS)
}

function flush() {
  timer = null
  if (!dirty) return
  dirty = false
  const d = dragging.value
  emit('aim', { ...(d.az !== undefined ? { azimuth_deg: d.az } : {}), ...(d.el !== undefined ? { elevation_deg: d.el } : {}) })
}

function startDrag(e: PointerEvent, what: 'az' | 'el') {
  which = what
  svg = e.currentTarget as SVGSVGElement
  svg.setPointerCapture(e.pointerId)
  svg.addEventListener('pointermove', onMove)
  svg.addEventListener('pointerup', onUp, { once: true })
  svg.addEventListener('pointercancel', onUp, { once: true })
  take(valueAt(e))
}

function onMove(e: PointerEvent) { take(valueAt(e)) }

function onUp() {
  svg?.removeEventListener('pointermove', onMove)
  if (timer) { clearTimeout(timer); timer = null }
  flush()
  which = null
  svg = null
  // The node's own figure takes over once the edit has come back.
  setTimeout(() => { if (!which) dragging.value = {} }, 400)
}

function nudge(what: 'az' | 'el', sign: number, big: boolean) {
  const step = (big ? 5 : 1) * sign
  if (what === 'az') emit('aim', { azimuth_deg: ((shownAz.value + step) % 360 + 360) % 360 })
  else emit('aim', { elevation_deg: Math.max(-90, Math.min(90, shownEl.value + step)) })
}

onBeforeUnmount(() => { if (timer) clearTimeout(timer) })
</script>

<style scoped>
.aim { display: flex; flex-wrap: wrap; gap: 6px 14px; align-items: flex-start; }
.aim-fig { margin: 0; display: flex; flex-direction: column; align-items: center; }
.aim-svg { width: 118px; height: 118px; cursor: crosshair; touch-action: none; outline: none; border-radius: 6px; }
.aim-svg:focus-visible { box-shadow: 0 0 0 1px #38bdf8; }
.aim-side { width: 71px; }
.aim-face { fill: #151a21; stroke: #2b3340; stroke-width: 1; }
.aim-major { stroke: #6b7280; stroke-width: 1.2; }
.aim-minor { stroke: #374151; stroke-width: 0.8; }
.aim-letter { fill: #9ca3af; font-size: 9px; font-family: ui-sans-serif, sans-serif; }
.aim-small { font-size: 7px; }
.aim-horizon { stroke: #475569; stroke-dasharray: 3 2; stroke-width: 0.8; }
.aim-mast { stroke: #6b7280; stroke-width: 2; }
.aim-beam { fill: rgba(56, 189, 248, 0.22); stroke: rgba(56, 189, 248, 0.5); stroke-width: 0.6; }
.aim-needle { stroke: #38bdf8; stroke-width: 2.2; stroke-linecap: round; }
.aim-hub { fill: #e5e7eb; }
figcaption { font-size: 11px; color: #9ca3af; }
figcaption b { color: #e5e7eb; font-weight: 500; font-family: ui-monospace, monospace; }
.aim-mixed { color: #f59e0b; }
.aim-hint { font-size: 10px; color: #6b7280; width: 100%; }
</style>
