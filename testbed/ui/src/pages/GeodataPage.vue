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
        <q-btn flat dense no-caps label="Export zip" type="a" :href="`/api/geodata/export?name=${encodeURIComponent(preview)}`"
               :download="`${preview}.zip`" />
        <PlaceSearch @go="p => map?.goTo(p.x, p.y, 1500)" />
        <DisplayMenu view="geodata" />
      </q-toolbar>
      <div class="gp-map">
        <GroundMap ref="map" :display="display.geodata" view-key="ground" />
      </div>
    </template>

    <BuildView v-else-if="building" @back="building = false" @started="building = false" />

    <div v-else class="gp-body">
      <div class="gp-head">
        <div class="gp-heading">Geodata</div>
        <q-space />
        <q-btn flat dense no-caps label="New synthetic…" @click="creating = true" />
        <q-btn flat dense no-caps label="Build from sources…" @click="building = true" />
        <q-btn unelevated dense no-caps color="primary" label="Import zip…" @click="importing = true" />
      </div>
      <div class="gp-text">
        The ground nodes stand on: a pack (terrain, clutter, buildings, roads,
        places, with ITU-R P.1812 over the real profile), built here from public
        sources, or synthetic ground at 0°, 0° whose degrees are metres at a nautical
        mile to the minute. Open one to work on it: the Nodes tab shows the nodesets
        with a node on it (counted here), and nothing before one is chosen.
      </div>
      <table class="gp-table">
        <thead><tr><th>Geodata</th><th>What</th><th>Extent</th><th>Nodesets</th><th></th></tr></thead>
        <tbody>
          <tr v-if="catalog.build" class="gp-building">
            <td><span class="gp-name">{{ catalog.build.name }}</span></td>
            <td colspan="4">
              <div class="gp-build">
                <span v-if="catalog.build.state === 'failed'" class="gp-bad">
                  build failed: {{ catalog.build.error }}
                </span>
                <span v-else>building… {{ progress(catalog.build) }}</span>
                <q-space />
                <q-btn flat dense no-caps size="sm"
                       :label="catalog.build.state === 'failed' ? 'Dismiss' : 'Cancel'" @click="cancelBuild" />
              </div>
              <q-linear-progress v-if="catalog.build.state !== 'failed'" :value="fraction(catalog.build) ?? 0"
                                 :indeterminate="fraction(catalog.build) === null" color="primary" class="q-mt-xs" />
            </td>
          </tr>
          <tr v-for="g in catalog.geodata" :key="g.name"
              :class="{ 'gp-row': !g.error, 'gp-chosen': g.name === nodes.geodata }"
              @click="!g.error && choose(g.name)">
            <td>
              <span class="gp-name">{{ g.name }}</span>
              <span v-if="g.name === nodes.geodata" class="gp-sub">chosen</span>
              <div v-if="g.error" class="gp-bad">{{ g.error }}</div>
            </td>
            <td>{{ describe(g) }}</td>
            <td class="mono">{{ extent(g) }}</td>
            <td class="mono">{{ g.error ? '' : g.nodesets ?? '…' }}</td>
            <td class="gp-act" @click.stop>
              <q-btn flat dense round size="sm" :icon="matEdit" @click="askRename(g.name)">
                <q-tooltip>Rename</q-tooltip>
              </q-btn>
              <q-btn flat dense round size="sm" :icon="matDeleteOutline" @click="askDelete(g.name)">
                <q-tooltip>Delete, with its pack</q-tooltip>
              </q-btn>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <q-dialog v-model="importing">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Import a zip</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-file v-model="file" dense outlined label="zip" accept=".zip,application/zip"
                  @update:model-value="nameFromZip" />
          <q-input v-model="name" dense outlined label="geodata name"
                   hint="lower-case letters, digits and hyphens" />
          <div class="text-caption text-grey-6">
            A SIMesh geodata pack, as Export zip writes one, or a bare planner pack
            (its manifest.json at the top or inside one directory). A pack goes into
            packs/, and becomes geodata by this name. Nodes are never ground: a pack's
            Nodes layer is left out.
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
/* The ground there is, and the three ways of getting more: New synthetic…,
 * Build from sources… (its own view, BuildView, and then a row here while it
 * builds), and Import zip…. A row opens that geodata on its own, with no
 * nodes on it, and from there Export zip takes it elsewhere. */
import { ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import { matDeleteOutline, matEdit } from '@quasar/extras/material-icons'
import GroundMap from '../components/GroundMap.vue'
import DisplayMenu from '../components/DisplayMenu.vue'
import PlaceSearch from '../components/PlaceSearch.vue'
import BuildView from '../components/BuildView.vue'
import { useCatalog, type BuildRow, type GeodataInfo } from '../stores/catalog'
import { zipGeodataName } from '../lib/zip'
import { useGeodata } from '../stores/geodata'
import { useDisplay } from '../stores/display'
import { useNodes } from '../stores/nodes'
import { useSim } from '../stores/sim'
import { request, upload } from '../lib/front'
import { whenSaved } from '../lib/unsaved'

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
const building = ref(false)

function describe(g: GeodataInfo | null) {
  if (!g || g.error) return ''
  if (g.kind === 'pack') return `pack, EPSG:${g.crs_epsg ?? '?'}${g.layers?.length ? ` · ${g.layers.length} layers` : ''}`
  return `synthetic, ${g.terrain ?? 'flat'}, exponent ${g.exponent}`
}

function extent(g: GeodataInfo) {
  if (g.error || !g.bbox) return ''
  if (g.kind !== 'pack') return `${((g.extent_m ?? 0) / 1000).toFixed(0)} km square at 0°, 0°`
  const [lon0, lat0, lon1, lat1] = g.bbox
  return `${lat0.toFixed(3)}…${lat1.toFixed(3)} N, ${lon0.toFixed(3)}…${lon1.toFixed(3)} E`
}

/* Opening a geodata chooses it: it is the ground the Nodes tab works on.
 * Another one leaves the nodeset being edited, so the page asks first about
 * unsaved edits. */
function choose(n: string) {
  whenSaved(quasar, () => { void show(n) })
}

async function show(n: string) {
  await nodes.chooseGeodata(n)
  preview.value = n
  const error = await ground.open(n)
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 6000 })
}

function back() { preview.value = null }

function tell(error: string | null | undefined, done?: string) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 6000 })
  else if (done) quasar.notify({ type: 'positive', message: done, timeout: 4000 })
}

/* Renaming or deleting the chosen geodata leaves the nodeset being edited too. */
function askRename(n: string) {
  const go = () => quasar.dialog({
    title: `Rename ${n}`, message: 'Lower-case letters, digits and hyphens.',
    prompt: { model: n, type: 'text' }, cancel: true,
  }).onOk(async (to: string) => {
    to = to.trim()
    if (!to || to === n) return
    const r = await request('geodata_rename', { name: n, to })
    if (!r.ok) { tell(r.error); return }
    await catalog.refreshGeodata()
    if (nodes.geodata === n) { await nodes.chooseGeodata(null); await show(to) }
    tell(null, `renamed to ${to}`)
  })
  if (nodes.geodata === n) whenSaved(quasar, go)
  else go()
}

function askDelete(n: string) {
  const go = () => quasar.dialog({
    title: `Delete ${n}`,
    message: `Delete the geodata ${n}, and its pack when nothing else uses it? This cannot be undone. `
      + 'Nodesets are not touched.',
    ok: { label: 'Delete', color: 'negative', flat: true, noCaps: true },
    cancel: { flat: true, noCaps: true }, persistent: true,
  }).onOk(async () => {
    const r = await request('geodata_delete', { name: n })
    if (!r.ok) { tell(r.error); return }
    if (nodes.geodata === n) { await nodes.chooseGeodata(null); preview.value = null; await ground.open(null) }
    await catalog.refreshGeodata()
    const kept = r.kept_by as string[]
    tell(null, kept.length ? `deleted; the pack stays, used by ${kept.join(', ')}` : 'deleted')
  })
  if (nodes.geodata === n) whenSaved(quasar, go)
  else go()
}

function mb(bytes: number) { return `${(bytes / 1e6).toFixed(bytes < 1e7 ? 1 : 0)} MB` }

/* A build's row: its step, and how far into it. Downloading counts bytes
 * where every host said a size, files where one did not; compiling counts
 * the compiler's steps. */
function progress(b: BuildRow) {
  const step = b.step ?? b.state
  if (b.state === 'fetching' && b.total) {
    const bytes = b.of ? ` (${mb(b.fetched)} of ${mb(b.of)})` : b.fetched ? ` (${mb(b.fetched)})` : ''
    return `${step} ${Math.min(b.done + 1, b.total)}/${b.total}${bytes}`
  }
  if (b.state === 'compiling' && b.total) return `${step} ${b.done + 1}/${b.total}`
  return step
}

function fraction(b: BuildRow): number | null {
  if (b.state === 'fetching' && b.of) return Math.min(1, b.fetched / b.of)
  if (b.state === 'fetching' && b.total) return b.done / b.total
  if (b.state === 'compiling' && b.total) return b.done / b.total
  return null
}

async function cancelBuild() {
  await fetch('/api/geodata/build', { method: 'DELETE' })
  if (catalog.build?.state === 'failed') catalog.build = null
}

async function nameFromZip(f: File | null) {
  if (!f) return
  const given = await zipGeodataName(f)
  if (given && file.value === f) name.value = given
}

/* Coming back to this tab puts the preview's ground back on the map, and
 * the nodeset counts are asked again. */
watch(() => sim.view, (v) => {
  if (v !== 'geodata') return
  void catalog.refreshGeodata()
  if (preview.value) void ground.open(preview.value)
})

async function doImport() {
  if (!file.value) return
  busy.value = true
  const r = await upload('/api/geodata/import', name.value.trim(), file.value)
  busy.value = false
  if (!r.ok) { quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 8000 }); return }
  importing.value = false
  file.value = null
  await catalog.refreshGeodata()
  choose(name.value.trim())
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
.gp-chosen td { background: #1a2130; }
.gp-act { white-space: nowrap; text-align: right; width: 1%; }
.gp-bad { font-size: 11px; color: #fca5a5; }
.gp-building td { background: #161a20; }
.gp-build { display: flex; align-items: center; gap: 8px; font-size: 12px; color: #d1d5db; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
</style>
