<template>
  <q-select v-if="ground.sidecar" ref="box" dense outlined options-dense use-input hide-selected fill-input
            input-debounce="250" :options="hits" :option-label="label" class="place-search"
            placeholder="find a place" :model-value="null" @filter="find"
            @update:model-value="pick">
    <template #no-option>
      <q-item dense><q-item-section class="text-grey-6">nothing by that name</q-item-section></q-item>
    </template>
  </q-select>
</template>

<script setup lang="ts">
/* Search the pack's places (streets, stations, districts, postcodes) by
 * name, through the sidecar's /search, and go there. */
import { ref } from 'vue'
import { useGeodata } from '../stores/geodata'
import { search, type Place } from '../lib/planner'

const emit = defineEmits<{ go: [place: Place] }>()
const ground = useGeodata()
const hits = ref<Place[]>([])
let request: AbortController | null = null

function label(p: Place | null) {
  return p ? `${p.name}${p.ctx ? `, ${p.ctx}` : ''} · ${p.kind}` : ''
}

function find(text: string, update: (fn: () => void) => void, abort: () => void) {
  const base = ground.sidecar
  if (!base || text.trim().length < 2) { abort(); return }
  request?.abort()
  const ctrl = new AbortController()
  request = ctrl
  search(base, text.trim(), ctrl.signal)
    .then(found => update(() => { hits.value = found }))
    .catch(() => abort())
}

function pick(p: Place | null) {
  if (p) emit('go', p)
}
</script>

<style scoped>
.place-search { width: 220px; }
</style>
