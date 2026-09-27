<template>
  <q-page class="gp">
    <template v-if="preview">
      <q-toolbar class="gp-bar">
        <q-btn flat dense no-caps label="‹ Back" @click="back" />
        <div class="gp-title">
          {{ preview }}
          <span class="gp-sub">{{ describe(ground.current) }}</span>
        </div>
        <q-space />
        <PlaceSearch @go="p => map?.goTo(p.x, p.y, 1500)" />
        <DisplayMenu view="geodata" />
      </q-toolbar>
      <div class="gp-map">
        <GroundMap ref="map" :display="display.geodata" view-key="preview" />
      </div>
    </template>

    <div v-else class="gp-body">
      <div class="gp-head">
        <div class="gp-heading">Geodata</div>
        <q-space />
        <q-btn flat dense no-caps label="New synthetic…" @click="creating = true" />
        <q-btn unelevated dense no-caps color="primary" label="Import…" @click="importing = true" />
      </div>
      <div class="gp-text">
        The ground nodes stand on: a planner pack (terrain, clutter, buildings,
        roads, places, with ITU-R P.1812 over the real profile), or synthetic
        ground at 0°, 0° whose degrees are metres at a nautical mile to the minute.
        Open one to look at it; the Nodes tab puts a nodeset on it.
      </div>
      <table class="gp-table">
        <thead><tr><th>Geodata</th><th>What</th><th>Extent</th></tr></thead>
        <tbody>
          <tr v-for="g in catalog.geodata" :key="g.name" :class="{ 'gp-row': !g.error }"
              @click="!g.error && show(g.name)">
            <td>
              <span class="gp-name">{{ g.name }}</span>
              <div v-if="g.error" class="gp-bad">{{ g.error }}</div>
            </td>
            <td>{{ describe(g) }}</td>
            <td class="mono">{{ extent(g) }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <q-dialog v-model="importing">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Import a planner pack</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-file v-model="file" dense outlined label="pack zip" accept=".zip,application/zip" />
          <q-input v-model="name" dense outlined label="geodata name"
                   hint="lower-case letters, digits and hyphens" />
          <div class="text-caption text-grey-6">
            A zip of a pack directory, its manifest.json at the top or inside one
            directory. It goes into the planner's packs, and becomes geodata by this name.
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Import" :loading="busy"
                 :disable="!file || !name.trim()" @click="doImport" />
        </q-card-actions>
      </q-card>
    </q-dialog>

    <q-dialog v-model="creating">
      <q-card style="min-width: 380px">
        <q-card-section class="text-subtitle2">New synthetic ground</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-input v-model="name" dense outlined label="name" />
          <q-input v-model.number="exponent" type="number" step="0.1" dense outlined
                   label="path-loss exponent" hint="2 free space, 2.7 suburban, 3.5 built-up" />
          <q-input v-model.number="extentKm" type="number" dense outlined label="extent (km across)" />
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Create" :disable="!name.trim()" @click="create" />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </q-page>
</template>

<script setup lang="ts">
/* The ground there is: listed, imported, made, and looked at on its own,
 * with no nodes on it. An editor for making and adapting it is to come, on
 * this same view. */
import { ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import GroundMap from '../components/GroundMap.vue'
import DisplayMenu from '../components/DisplayMenu.vue'
import PlaceSearch from '../components/PlaceSearch.vue'
import { useCatalog, type GeodataInfo } from '../stores/catalog'
import { useGeodata } from '../stores/geodata'
import { useDisplay } from '../stores/display'
import { useNodes } from '../stores/nodes'
import { useSim } from '../stores/sim'
import { request, upload } from '../lib/front'

const catalog = useCatalog()
const ground = useGeodata()
const display = useDisplay()
const nodes = useNodes()
const sim = useSim()
const quasar = useQuasar()
const map = ref<InstanceType<typeof GroundMap>>()
const preview = ref<string | null>(null)
const importing = ref(false)
const creating = ref(false)
const file = ref<File | null>(null)
const name = ref('')
const exponent = ref(2.7)
const extentKm = ref(20)
const busy = ref(false)

function describe(g: GeodataInfo | null) {
  if (!g || g.error) return ''
  if (g.kind === 'pack') return `planner pack, EPSG:${g.crs_epsg ?? '?'}${g.layers?.length ? ` · ${g.layers.length} layers` : ''}`
  return `synthetic, ${g.terrain ?? 'flat'}, exponent ${g.exponent}`
}

function extent(g: GeodataInfo) {
  if (g.error || !g.bbox) return ''
  if (g.kind !== 'pack') return `${((g.extent_m ?? 0) / 1000).toFixed(0)} km square at 0°, 0°`
  const [lon0, lat0, lon1, lat1] = g.bbox
  return `${lat0.toFixed(3)}…${lat1.toFixed(3)} N, ${lon0.toFixed(3)}…${lon1.toFixed(3)} E`
}

/* The geodata looked at last is the one the Nodes tab lists nodesets for,
 * unless it has one open already. */
async function show(n: string) {
  preview.value = n
  if (!nodes.open) nodes.geodata = n
  const error = await ground.open(n)
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 6000 })
}

function back() { preview.value = null }

/* Coming back to this tab puts the preview's ground back on the map. */
watch(() => sim.view, (v) => { if (v === 'geodata' && preview.value) void ground.open(preview.value) })

async function doImport() {
  if (!file.value) return
  busy.value = true
  const r = await upload('/api/geodata/import', name.value.trim(), file.value)
  busy.value = false
  if (!r.ok) { quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 8000 }); return }
  importing.value = false
  file.value = null
  await catalog.refresh()
  await show(name.value.trim())
}

async function create() {
  const r = await request('geodata_new', {
    name: name.value.trim(),
    synthetic: { terrain: 'flat', exponent: exponent.value, extent_m: extentKm.value * 1000 },
  })
  if (!r.ok) { quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 6000 }); return }
  creating.value = false
  await catalog.refresh()
}
</script>

<style scoped>
.gp { display: flex; flex-direction: column; height: 100%; }
.gp-bar { min-height: 38px; gap: 6px; padding-left: 4px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.gp-title { font-size: 14px; font-weight: 500; padding: 0 10px; }
.gp-sub { font-size: 12px; font-weight: 400; color: #6b7280; padding-left: 8px; }
.gp-map { position: relative; flex: 1 1 auto; min-height: 0; }
.gp-body { padding: 16px 20px 32px; max-width: 1100px; overflow-y: auto; }
.gp-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.gp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.gp-text { font-size: 12px; color: #9ca3af; line-height: 1.5; margin-bottom: 12px; max-width: 760px; }
.gp-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.gp-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35;
}
.gp-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.gp-name { font-weight: 500; }
.gp-row { cursor: pointer; }
.gp-row .gp-name { color: #7dd3fc; }
.gp-row:hover td { background: #1b2028; }
.gp-bad { font-size: 11px; color: #fca5a5; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
</style>
