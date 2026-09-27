<template>
  <FloatingWindow id="pair-inspector" :title="`${a.name} → ${b.name}`" :visible="true"
                  :default-geom="{ x: 2, y: 52, w: 46, h: 44 }" :min-size="{ w: 24, h: 24 }"
                  @update:visible="v => !v && $emit('close')">
    <div class="pair">
      <div v-if="!sidecar" class="pair-note">
        Synthetic ground has no terrain to profile: its loss is log-distance, plus
        whatever offset the nodeset gives the pair, and the loss table is all there
        is to inspect.
      </div>
      <template v-else>
        <div class="pair-head">
          <span v-if="loading">asking the planner…</span>
          <span v-else-if="error" class="pair-error">{{ error }}</span>
          <template v-else-if="reply">
            <b>{{ reply.lb_db.toFixed(1) }} dB</b>
            <span>{{ reply.distance_km.toFixed(2) }} km</span>
            <span :class="`pair-${reply.fresnel.verdict}`">Fresnel {{ reply.fresnel.verdict }}</span>
            <span>{{ model }}</span>
            <span class="pair-dim">
              antennas {{ reply.tx_h.toFixed(1) }} m<template v-if="reply.tx_indoor_entry_db != null">
              (indoors, +{{ reply.tx_indoor_entry_db.toFixed(0) }} dB walls)</template>
              and {{ reply.rx_h.toFixed(1) }} m<template v-if="reply.rx_indoor_entry_db != null">
              (indoors, +{{ reply.rx_indoor_entry_db.toFixed(0) }} dB walls)</template>
              above ground
            </span>
          </template>
          <q-space />
          <span v-if="tableCell" class="pair-dim">table {{ tableCell }}</span>
          <q-btn flat dense no-caps size="sm" label="Reverse" @click="$emit('reverse')" />
        </div>
        <canvas ref="chart" class="pair-chart" />
      </template>
      <!-- An offset on this pair: loss the model misses, both ways. -->
      <div class="pair-offset">
        <q-input v-model.number="offsetDb" type="number" dense outlined label="extra loss (dB)"
                 class="pair-offset-db">
          <q-tooltip>Added to the computed loss both ways, for what the model misses (a wall, a tree); negative lowers it</q-tooltip>
        </q-input>
        <q-input v-model="offsetNote" dense outlined label="why" class="col" />
        <q-btn flat dense no-caps size="sm" label="Apply" :disable="!offsetChanged"
               @click="nodes.setOffset(a.name, b.name, offsetDb || 0, offsetNote)" />
      </div>
    </div>
  </FloatingWindow>
</template>

<script setup lang="ts">
/* The pair inspector: link.json between two nodes, the planner's own
 * answer for the pair, with its terrain profile drawn. The loss table's cell
 * for the same pair is shown beside it when there is a table, and the two
 * agree, near-field pairs included, because the table is built from these
 * very replies. */
import { computed, nextTick, onUnmounted, ref, watch } from 'vue'
import FloatingWindow from './FloatingWindow.vue'
import { link, type LinkReply } from '../lib/planner'
import { useGeodata } from '../stores/geodata'
import { useNodes } from '../stores/nodes'

export interface PairEnd { name: string; lat: number; lon: number; height_m: number; gain_dbi: number }

const props = defineProps<{ a: PairEnd; b: PairEnd; tableCell?: string | null }>()
defineEmits<{ close: []; reverse: [] }>()

const ground = useGeodata()
const nodes = useNodes()
const sidecar = computed(() => ground.sidecar)

/* The pair's offset, in either order: shown, edited, and applied here. */
const heldOffset = computed(() => nodes.offsets.find(o =>
  o.between.includes(props.a.name) && o.between.includes(props.b.name)))
const offsetDb = ref(0)
const offsetNote = ref('')
watch(heldOffset, (o) => { offsetDb.value = o?.db ?? 0; offsetNote.value = o?.note ?? '' }, { immediate: true })
const offsetChanged = computed(() => (offsetDb.value || 0) !== (heldOffset.value?.db ?? 0)
  || offsetNote.value !== (heldOffset.value?.note ?? ''))
const reply = ref<LinkReply | null>(null)
const error = ref<string | null>(null)
const loading = ref(false)
const chart = ref<HTMLCanvasElement>()
let request: AbortController | null = null

const model = computed(() => {
  const m = reply.value?.profile_evidence?.model
  return typeof m === 'string' ? m : reply.value && reply.value.distance_km < 0.25 ? 'near-field' : ''
})

async function ask() {
  const base = sidecar.value
  if (!base) return
  request?.abort()
  const ctrl = new AbortController()
  request = ctrl
  loading.value = true
  error.value = null
  const f = ground.frame
  try {
    const got = await link(base, f.toXY(props.a.lat, props.a.lon), f.toXY(props.b.lat, props.b.lon),
                           props.a.height_m, props.b.height_m, props.a.gain_dbi, props.b.gain_dbi, ctrl.signal)
    if (ctrl.signal.aborted) return
    reply.value = got
  } catch (e) {
    if ((e as Error).name === 'AbortError') return
    reply.value = null
    error.value = (e as Error).message
  } finally {
    if (request === ctrl) loading.value = false
  }
  await nextTick()
  drawProfile()
}

/* The profile: terrain filled, the surface with clutter and buildings over
 * it, the ray between the antennas and its first Fresnel zone below it. */
function drawProfile() {
  const cv = chart.value
  const p = reply.value?.profile
  if (!cv || !p?.length) return
  const dpr = window.devicePixelRatio || 1
  const w = cv.clientWidth, h = cv.clientHeight
  cv.width = Math.round(w * dpr)
  cv.height = Math.round(h * dpr)
  const c = cv.getContext('2d')!
  c.setTransform(dpr, 0, 0, dpr, 0, 0)
  c.fillStyle = '#0e1116'
  c.fillRect(0, 0, w, h)
  const pad = { l: 40, r: 10, t: 10, b: 20 }
  const dMax = p[p.length - 1]!.d || 1
  let lo = Infinity, hi = -Infinity
  for (const q of p) {
    lo = Math.min(lo, q.h, q.ray - q.f1)
    hi = Math.max(hi, q.g, q.ray)
  }
  const span = Math.max(hi - lo, 10)
  lo -= span * 0.05
  hi += span * 0.1
  const X = (d: number) => pad.l + d / dMax * (w - pad.l - pad.r)
  const Y = (m: number) => pad.t + (hi - m) / (hi - lo) * (h - pad.t - pad.b)
  const area = (key: 'h' | 'g', colour: string) => {
    c.beginPath()
    c.moveTo(X(0), Y(lo))
    for (const q of p) c.lineTo(X(q.d), Y(q[key]))
    c.lineTo(X(dMax), Y(lo))
    c.closePath()
    c.fillStyle = colour
    c.fill()
  }
  area('g', '#4b5563')
  area('h', '#7c5a3a')
  c.strokeStyle = 'rgba(56, 189, 248, 0.5)'
  c.setLineDash([4, 3])
  c.beginPath()
  p.forEach((q, i) => (i ? c.lineTo(X(q.d), Y(q.ray - q.f1)) : c.moveTo(X(q.d), Y(q.ray - q.f1))))
  c.stroke()
  c.setLineDash([])
  c.strokeStyle = '#38bdf8'
  c.lineWidth = 1.5
  c.beginPath()
  p.forEach((q, i) => (i ? c.lineTo(X(q.d), Y(q.ray)) : c.moveTo(X(q.d), Y(q.ray))))
  c.stroke()
  c.fillStyle = '#9ca3af'
  c.font = '10px ui-monospace, monospace'
  c.textAlign = 'right'
  for (const m of [lo, (lo + hi) / 2, hi]) c.fillText(`${Math.round(m)} m`, pad.l - 4, Y(m) + 3)
  c.textAlign = 'center'
  c.fillText(`${dMax.toFixed(2)} km`, X(dMax) - 20, h - 6)
  c.fillText('0', X(0) + 4, h - 6)
}

watch(() => [props.a, props.b, sidecar.value], ask, { immediate: true, deep: true })
let observer: ResizeObserver | null = null
watch(chart, (cv) => {
  observer?.disconnect()
  if (cv) { observer = new ResizeObserver(drawProfile); observer.observe(cv) }
})
onUnmounted(() => { request?.abort(); observer?.disconnect() })
</script>

<style scoped>
.pair { display: flex; flex-direction: column; height: 100%; padding: 6px 4px; gap: 6px; background: #0e1116; }
.pair-head { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; font-size: 12px; color: #d1d5db; }
.pair-head b { font-size: 14px; color: #fff; }
.pair-dim { color: #9ca3af; }
.pair-error { color: #fca5a5; }
.pair-note { font-size: 12px; color: #9ca3af; padding: 8px; }
.pair-clear { color: #22c55e; }
.pair-grazing { color: #f59e0b; }
.pair-obstructed { color: #ef4444; }
.pair-chart { flex: 1; min-height: 0; width: 100%; }
.pair-offset { display: flex; align-items: center; gap: 6px; flex: none; padding: 0 4px; }
.pair-offset-db { width: 120px; }
</style>
