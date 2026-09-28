<template>
  <q-input v-if="nominatim" v-model="text" dense outlined class="place-search" placeholder="find a place ⏎"
           :loading="asking" @keyup.enter="ask">
    <q-menu v-model="listing" no-focus fit anchor="bottom left" self="top left">
      <q-list dense style="max-width: 420px">
        <q-item v-for="(p, i) in found" :key="i" v-close-popup clickable @click="emit('found', p)">
          <q-item-section class="ellipsis">{{ p.name }}</q-item-section>
        </q-item>
        <q-item v-if="!found.length">
          <q-item-section class="text-grey-6">{{ problem ?? 'nothing by that name' }}</q-item-section>
        </q-item>
      </q-list>
    </q-menu>
  </q-input>
  <q-select v-else-if="ground.sidecar" ref="box" dense outlined options-dense use-input hide-selected fill-input
            input-debounce="250" :options="hits" :option-label="label" class="place-search"
            placeholder="find a place" :model-value="null" @filter="find"
            @update:model-value="pick">
    <template #no-option>
      <q-item dense><q-item-section class="text-grey-6">nothing by that name</q-item-section></q-item>
    </template>
  </q-select>
</template>

<script setup lang="ts">
/* Find a place by name and go there. On a pack: its places (streets,
 * stations, districts, postcodes), through the sidecar's /search, as one
 * types. With `nominatim`: anywhere, through the front's /api/nominatim,
 * OpenStreetMap's geocoder, whose usage policy allows one request per search
 * and no searching as one types, so it asks when Enter is pressed. */
import { ref } from 'vue'
import { useGeodata } from '../stores/geodata'
import { search, type Place } from '../lib/planner'

export interface FoundPlace { name: string; lat: number; lon: number; bbox: [number, number, number, number] }

const props = withDefaults(defineProps<{ nominatim?: boolean }>(), { nominatim: false })
const emit = defineEmits<{ go: [place: Place]; found: [place: FoundPlace] }>()
const ground = useGeodata()
const hits = ref<Place[]>([])
let request: AbortController | null = null

const text = ref('')
const found = ref<FoundPlace[]>([])
const listing = ref(false)
const asking = ref(false)
const problem = ref<string | null>(null)

function label(p: Place | null) {
  return p ? `${p.name}${p.ctx ? `, ${p.ctx}` : ''} · ${p.kind}` : ''
}

function find(t: string, update: (fn: () => void) => void, abort: () => void) {
  const base = ground.sidecar
  if (!base || t.trim().length < 2) { abort(); return }
  request?.abort()
  const ctrl = new AbortController()
  request = ctrl
  search(base, t.trim(), ctrl.signal)
    .then(got => update(() => { hits.value = got }))
    .catch(() => abort())
}

function pick(p: Place | null) {
  if (p) emit('go', p)
}

async function ask() {
  const q = text.value.trim()
  if (!props.nominatim || !q || asking.value) return
  asking.value = true
  problem.value = null
  try {
    const r = await fetch(`/api/nominatim?q=${encodeURIComponent(q)}`)
    const body = await r.json() as { ok: boolean; places?: FoundPlace[]; error?: string }
    found.value = body.places ?? []
    if (!body.ok) problem.value = body.error ?? 'Nominatim did not answer'
    if (found.value.length === 1) emit('found', found.value[0]!)
    else listing.value = true
  } catch (e) {
    found.value = []
    problem.value = (e as Error).message
    listing.value = true
  } finally {
    asking.value = false
  }
}
</script>

<style scoped>
.place-search { width: 220px; }
</style>
