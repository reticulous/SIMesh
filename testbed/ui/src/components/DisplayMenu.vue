<template>
  <q-btn flat dense round size="sm" class="display-btn" aria-label="Display">
    <span class="display-burger">☰</span>
    <q-tooltip>How the map is shown</q-tooltip>
    <q-menu anchor="bottom right" self="top right"
            @show="display.menuOpen = true" @hide="display.menuOpen = false">
      <q-list dense style="min-width: 230px">
        <template v-if="ground.isPack">
          <q-item-label header>Ground, shaded by</q-item-label>
          <q-item dense>
            <q-option-group :model-value="d.base" :options="BASES" type="radio" dense inline
                            @update:model-value="(v: BaseLayer) => set({ base: v })" />
          </q-item>
          <toggle label="Roads and railways" field="roads" />
          <toggle label="Buildings" field="buildings" />
          <q-item-label header class="display-legend">
            Buildings: outlines under 9 km across, filled by height under 4 km
            (slate low, sand 20 m, red 60 m and up).
          </q-item-label>
          <q-separator />
          <q-item-label header>Heatmap</q-item-label>
          <q-item dense>
            <q-option-group :model-value="heat" :options="HEATMAPS" type="radio" dense
                            @update:model-value="(v: Heat) => setHeat(v)" />
          </q-item>
          <q-item-label header class="display-legend">
            One at a time; under it, the rest of the map is grey.
          </q-item-label>
        </template>
        <template v-else>
          <q-item-label header>Grid in</q-item-label>
          <q-item dense>
            <q-option-group :model-value="d.units" :options="UNITS" type="radio" dense
                            @update:model-value="(v: Display['units']) => set({ units: v })" />
          </q-item>
          <template v-if="view === 'nodes'">
            <q-separator />
            <q-item-label header>Heatmap</q-item-label>
            <q-item dense>
              <q-option-group :model-value="heat" :options="HEATMAPS.filter(h => h.value !== 'population')"
                              type="radio" dense @update:model-value="(v: Heat) => setHeat(v)" />
            </q-item>
          </template>
        </template>
        <template v-if="view === 'nodes'">
          <q-separator />
          <q-item-label header>Layers</q-item-label>
          <toggle label="Links from the selected node" field="links" />
          <toggle label="Offsets" field="offsets" />
          <q-separator />
          <q-item-label header>Nodes</q-item-label>
          <toggle label="Names" field="labels" />
          <toggle label="Antenna height above ground" field="heights" />
          <toggle label="Tags" field="tags" />
        </template>
      </q-list>
    </q-menu>
  </q-btn>
</template>

<script setup lang="ts">
/* How the map is shown: the pack's ground and its layers, or synthetic
 * ground's grid; and on the Nodes tab its layers and what each node is
 * labelled with. Kept per tab, per browser. */
import { computed, defineComponent, h } from 'vue'
import { QItem, QItemSection, QToggle } from 'quasar'
import { useDisplay, type Display, type View } from '../stores/display'
import { useGeodata } from '../stores/geodata'
import type { BaseLayer } from '../lib/planner'

const props = defineProps<{ view: View }>()
const display = useDisplay()
const ground = useGeodata()
const d = computed(() => display[props.view])
const BASES: { value: BaseLayer; label: string }[] = [
  { value: 'terrain', label: 'terrain' },
  { value: 'clutter', label: 'clutter height' },
]
const UNITS: { value: Display['units']; label: string }[] = [
  { value: 'metres', label: 'metres from 0°, 0°' }, { value: 'degrees', label: 'degrees' },
]

/* The heatmaps: none, or one of them. Coverage is the Nodes tab's only. */
type Heat = 'none' | 'population' | 'coverage'
const HEATMAPS = computed(() => [
  { value: 'none', label: 'none' },
  { value: 'population', label: 'population' },
  ...(props.view === 'nodes' ? [{ value: 'coverage', label: 'coverage' }] : []),
] as { value: Heat; label: string }[])
const heat = computed<Heat>(() => (d.value.population ? 'population'
  : props.view === 'nodes' && d.value.coverage ? 'coverage' : 'none'))
function setHeat(v: Heat) { set({ population: v === 'population', coverage: v === 'coverage' }) }

function set(change: Partial<Display>) { display.set(props.view, change) }

/* One on/off row of the menu. */
const toggle = defineComponent({
  props: { label: { type: String, required: true }, field: { type: String, required: true } },
  setup(p) {
    return () => h(QItem, { tag: 'label', dense: true }, () => [
      h(QItemSection, () => p.label),
      h(QItemSection, { side: true }, () => h(QToggle, {
        dense: true,
        modelValue: d.value[p.field as keyof Display] as boolean,
        'onUpdate:modelValue': (v: boolean) => set({ [p.field]: v } as Partial<Display>),
      })),
    ])
  },
})
</script>

<style scoped>
.display-btn { margin: 0 4px; }
.display-burger { font-size: 16px; line-height: 1; color: #d1d5db; }
.display-legend { font-size: 11px; line-height: 1.4; max-width: 230px; }
</style>
