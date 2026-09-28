<template>
  <q-page class="ap">
    <div class="ap-list">
      <div class="ap-heading">Antennas</div>
      <div class="ap-text">
        Generic representatives of the antennas an 868 MHz node carries. A
        node's antenna is one of these, chosen in the Nodes tab's editor; a
        directional one is aimed there too.
      </div>
      <div v-for="a in catalog.antennas" :key="a.type" class="ap-row"
           :class="{ 'ap-on': a.type === chosen?.type }" @click="pick = a.type">
        <div class="ap-pic" v-html="a.svg ?? ''" />
        <div class="ap-what">
          <div class="ap-label">{{ a.label }} <span class="ap-gain">{{ fmt(a.peak_dbi) }} dBi</span></div>
          <div class="ap-desc">{{ a.description }}</div>
        </div>
      </div>
    </div>

    <div v-if="chosen" class="ap-detail">
      <div class="ap-top">
        <div class="ap-big" v-html="chosen.svg ?? ''" />
        <div>
          <div class="ap-title">{{ chosen.label }}</div>
          <div class="ap-type mono">{{ chosen.type }}</div>
          <div class="ap-desc">{{ chosen.description }}</div>
          <table class="ap-figs">
            <tr><td>peak gain</td><td>{{ fmt(chosen.peak_dbi) }} dBi</td></tr>
            <tr><td>vertical beamwidth</td><td>{{ chosen.vbw_deg }}°</td></tr>
            <tr v-if="chosen.hbw_deg"><td>horizontal beamwidth</td><td>{{ chosen.hbw_deg }}°</td></tr>
            <tr><td>main lobe</td><td>{{ chosen.tilt_deg ? `${chosen.tilt_deg}° above the horizon` : 'on the horizon' }}</td></tr>
            <tr><td>floor</td><td>{{ chosen.floor_db }} dB below the peak</td></tr>
            <tr><td>kind</td><td>{{ chosen.kind === 'directional' ? 'directional, aimed by its node' : 'omnidirectional' }}</td></tr>
          </table>
        </div>
      </div>
      <div class="ap-plots">
        <figure>
          <svg :viewBox="`0 0 ${SIZE} ${SIZE}`" class="ap-polar">
            <g v-for="r in rings" :key="r.db">
              <circle :cx="C" :cy="C" :r="r.r" class="ap-ring" />
              <text :x="C + 3" :y="C - r.r + 11" class="ap-ringtext">{{ r.db }}</text>
            </g>
            <line v-for="s in spokes" :key="s.a" :x1="C" :y1="C" :x2="s.x" :y2="s.y" class="ap-spoke" />
            <text v-for="s in compass" :key="s.t" :x="s.x" :y="s.y" class="ap-dir">{{ s.t }}</text>
            <path :d="azPath" class="ap-trace" />
          </svg>
          <figcaption>Horizontal plane (azimuth), at the horizon{{ chosen.kind === 'directional' ? ', aimed north' : '' }}</figcaption>
        </figure>
        <figure>
          <svg :viewBox="`0 0 ${SIZE} ${SIZE}`" class="ap-polar">
            <g v-for="r in rings" :key="r.db">
              <circle :cx="C" :cy="C" :r="r.r" class="ap-ring" />
              <text :x="C + 3" :y="C - r.r + 11" class="ap-ringtext">{{ r.db }}</text>
            </g>
            <line v-for="s in spokes" :key="s.a" :x1="C" :y1="C" :x2="s.x" :y2="s.y" class="ap-spoke" />
            <text v-for="s in elevation" :key="s.t" :x="s.x" :y="s.y" class="ap-dir">{{ s.t }}</text>
            <line :x1="PAD" :y1="C" :x2="SIZE - PAD" :y2="C" class="ap-horizon" />
            <path :d="elPath" class="ap-trace" />
          </svg>
          <figcaption>Vertical plane (elevation){{ chosen.kind === 'directional' ? ', through its aim' : '' }}</figcaption>
        </figure>
      </div>
      <div class="ap-text">
        Gain in dBi, the rings 10 dB apart. The pattern is drawn from the
        figures above: 3 dB down at half a beamwidth, never lower than the
        floor. Between two nodes each antenna's gain is taken toward the
        other in three dimensions, from their antennas' heights over the
        ground.
      </div>
    </div>
  </q-page>
</template>

<script setup lang="ts">
/* The antenna catalogue, each with its picture, what it is, and its
 * radiation pattern in the horizontal and the vertical plane. */
import { computed, onMounted, ref } from 'vue'
import { useCatalog } from '../stores/catalog'
import { gain, type AntennaSpec } from '../lib/antennas'

const catalog = useCatalog()
const pick = ref<string | null>(null)

onMounted(() => { void catalog.ensureAntennas() })

const chosen = computed<AntennaSpec | null>(() =>
  catalog.antennas.find(a => a.type === pick.value) ?? catalog.antennas[0] ?? null)

const SIZE = 300
const PAD = 22
const C = SIZE / 2
const R = C - PAD
// The scale is the catalogue's: every antenna on the same rings.
const TOP = computed(() => Math.ceil(Math.max(0, ...catalog.antennas.map(a => a.peak_dbi)) / 10) * 10 + 5)
const SPAN = 40

function fmt(v: number) { return v > 0 ? `+${v}` : `${v}` }

function radius(db: number) {
  return Math.max(0, (db - (TOP.value - SPAN)) / SPAN) * R
}

const rings = computed(() => [0, 10, 20, 30].map(k => ({ db: TOP.value - 5 - k, r: radius(TOP.value - 5 - k) })))

const spokes = Array.from({ length: 12 }, (_, i) => {
  const a = i * 30 * Math.PI / 180
  return { a: i, x: C + R * Math.sin(a), y: C - R * Math.cos(a) }
})

function label(deg: number, t: string) {
  const a = deg * Math.PI / 180
  return { t, x: C + (R + 12) * Math.sin(a) - 4, y: C - (R + 12) * Math.cos(a) + 4 }
}
const compass = [label(0, 'N'), label(90, 'E'), label(180, 'S'), label(270, 'W')]
const elevation = [label(90, '0°'), label(0, '+90°'), label(270, 'back'), label(180, '−90°')]

function trace(point: (deg: number) => [number, number]) {
  const out: string[] = []
  for (let d = 0; d <= 360; d += 2) {
    const [x, y] = point(d)
    out.push(`${d ? 'L' : 'M'}${x.toFixed(1)},${y.toFixed(1)}`)
  }
  return out.join(' ') + ' Z'
}

// Azimuth: north up, clockwise, at the horizon; a directional one aimed north.
const azPath = computed(() => {
  const spec = chosen.value
  if (!spec) return ''
  const aim = { type: spec.type, azimuth_deg: 0, elevation_deg: 0 }
  return trace((d) => {
    const r = radius(gain(spec, aim, d, 0))
    const a = d * Math.PI / 180
    return [C + r * Math.sin(a), C - r * Math.cos(a)]
  })
})

// Elevation: the forward horizon on the right, up at the top, the back on the left.
const elPath = computed(() => {
  const spec = chosen.value
  if (!spec) return ''
  const aim = { type: spec.type, azimuth_deg: 0, elevation_deg: 0 }
  return trace((d) => {
    // d is the angle counter-clockwise from the forward horizon.
    const t = ((d + 180) % 360) - 180
    const forward = Math.abs(t) <= 90
    const el = forward ? t : (t > 0 ? 180 - t : -180 - t)
    const r = radius(gain(spec, aim, forward ? 0 : 180, el))
    const a = d * Math.PI / 180
    return [C + r * Math.cos(a), C - r * Math.sin(a)]
  })
})
</script>

<style scoped>
.ap { display: flex; overflow: hidden; height: 100%; }
.ap-list { width: 400px; flex: none; overflow-y: auto; padding: 16px 12px 32px 20px; border-right: 1px solid #1f242c; }
.ap-detail { flex: 1; overflow-y: auto; padding: 16px 24px 32px; }
.ap-heading { font-size: 14px; font-weight: 500; color: #d1d5db; margin-bottom: 6px; }
.ap-text { font-size: 12px; color: #9ca3af; line-height: 1.5; margin-bottom: 12px; max-width: 700px; }
.ap-row { display: flex; gap: 10px; padding: 8px; border-radius: 4px; cursor: pointer; align-items: flex-start; }
.ap-row:hover { background: #1a1f27; }
.ap-on { background: #1e293b; }
.ap-pic { width: 44px; height: 44px; flex: none; color: #cbd5e1; }
.ap-pic :deep(svg), .ap-big :deep(svg) { width: 100%; height: 100%; }
.ap-label { font-size: 13px; color: #e5e7eb; font-weight: 500; }
.ap-gain { font-size: 11px; color: #7dd3fc; font-weight: 400; margin-left: 4px; }
.ap-desc { font-size: 12px; color: #9ca3af; line-height: 1.4; }
.ap-top { display: flex; gap: 20px; align-items: flex-start; margin-bottom: 12px; }
.ap-big { width: 120px; height: 120px; flex: none; color: #e5e7eb; }
.ap-title { font-size: 16px; color: #f3f4f6; font-weight: 500; }
.ap-type { font-size: 12px; color: #6b7280; margin-bottom: 6px; }
.ap-figs { font-size: 12px; color: #d1d5db; margin-top: 10px; border-collapse: collapse; }
.ap-figs td { padding: 2px 14px 2px 0; }
.ap-figs td:first-child { color: #6b7280; }
.ap-plots { display: flex; gap: 24px; flex-wrap: wrap; margin: 8px 0 12px; }
.ap-plots figure { margin: 0; }
.ap-plots figcaption { font-size: 12px; color: #9ca3af; text-align: center; max-width: 300px; }
.ap-polar { width: 300px; height: 300px; }
.ap-ring { fill: none; stroke: #2b3340; stroke-width: 1; }
.ap-spoke { stroke: #1f2630; stroke-width: 1; }
.ap-horizon { stroke: #475569; stroke-width: 1; stroke-dasharray: 3 3; }
.ap-ringtext { fill: #6b7280; font-size: 9px; font-family: ui-monospace, monospace; }
.ap-dir { fill: #9ca3af; font-size: 10px; }
.ap-trace { fill: rgba(56, 189, 248, 0.18); stroke: #38bdf8; stroke-width: 1.6; }
.mono { font-family: ui-monospace, monospace; }
</style>
