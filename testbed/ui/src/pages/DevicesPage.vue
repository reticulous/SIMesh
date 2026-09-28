<template>
  <q-page class="dp">
    <div class="dp-body">
      <div class="dp-head">
        <div class="dp-heading">Latest builds</div>
        <q-space />
        <q-btn flat dense no-caps label="Refresh" :loading="refreshing" @click="refresh">
          <q-tooltip>Look for newer builds in the web's catalogues and in builds/ beside SIMesh</q-tooltip>
        </q-btn>
        <q-btn unelevated dense no-caps color="primary" label="Import…" @click="importing = true" />
      </div>
      <div class="dp-text">
        The newest build of each project in each catalogue, for this machine
        ({{ catalog.arch || '…' }}). A script names one as it is shown here,
        <code>firmware("all", "reticulous_dev_latest")</code>, and it is
        fetched when a simulation first uses it; a newer one in its catalogue
        replaces it. Save keeps the one there is now.
      </div>

      <table class="dp-table">
        <thead>
          <tr><th>Build</th><th>Hardware</th><th>Kind</th><th>Built</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="d in catalog.latest" :key="d.ref" :class="{ 'dp-off': d.error }">
            <td>
              <div class="dp-name mono">{{ d.ref }}</div>
              <div v-if="d.error" class="dp-bad">{{ d.error }}</div>
              <div v-else class="dp-sub">{{ whence(d) }}</div>
            </td>
            <td>
              <div>{{ d.virtual_hardware ? `virtual ${d.virtual_hardware}` : '—' }}</div>
              <div v-if="d.virtual_radio" class="dp-sub">virtual {{ d.virtual_radio }}</div>
            </td>
            <td class="mono">{{ d.kind ?? '—' }}</td>
            <td class="mono">{{ when(d.stamp) }}</td>
            <td class="dp-act">
              <q-btn flat dense no-caps size="sm" label="Save" :loading="saving === d.ref"
                     :disable="!!d.error || saved(d)" @click="save(d)">
                <q-tooltip>{{ saved(d) ? 'Saved already' : 'Keep this build, fetching it first when it is not here' }}</q-tooltip>
              </q-btn>
            </td>
          </tr>
          <tr v-if="!catalog.latest.length">
            <td colspan="5" class="dp-sub">No builds for this machine in any catalogue: Refresh, or Import a device file.</td>
          </tr>
        </tbody>
      </table>
      <div v-if="said.length" class="dp-said">
        <div v-for="(line, i) in said" :key="i">{{ line }}</div>
      </div>

      <div class="dp-head dp-second">
        <div class="dp-heading">Saved builds</div>
      </div>
      <div class="dp-text">
        Kept until deleted, named with their build time:
        <code>firmware("all", "reticulous_dev_20260927140352")</code>.
      </div>
      <table class="dp-table">
        <thead>
          <tr><th>Build</th><th>Hardware</th><th>Kind</th><th>Built</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="d in catalog.saved" :key="d.ref" :class="{ 'dp-off': d.error }">
            <td>
              <div class="dp-name mono">{{ d.ref }}</div>
              <div v-if="d.error" class="dp-bad">{{ d.error }}</div>
              <div v-else-if="d.name" class="dp-sub">{{ d.name }}</div>
            </td>
            <td>
              <div>{{ d.virtual_hardware ? `virtual ${d.virtual_hardware}` : '—' }}</div>
              <div v-if="d.virtual_radio" class="dp-sub">virtual {{ d.virtual_radio }}</div>
            </td>
            <td class="mono">{{ d.kind ?? '—' }}</td>
            <td class="mono">{{ when(d.stamp) }}</td>
            <td class="dp-act">
              <q-btn flat dense round size="sm" :icon="matDeleteOutline" @click="remove(d)">
                <q-tooltip>Delete this saved build</q-tooltip>
              </q-btn>
            </td>
          </tr>
          <tr v-if="!catalog.saved.length">
            <td colspan="5" class="dp-sub">Nothing saved yet.</td>
          </tr>
        </tbody>
      </table>
    </div>

    <q-dialog v-model="importing">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Import a device file</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-file v-model="file" dense outlined label="device zip" accept=".zip,application/zip" />
          <div class="text-caption text-grey-6">
            A zip with a node.yaml at its top, built for this machine. It is
            saved as <code>&lt;project&gt;_imported_&lt;stamp&gt;</code>.
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Import" :loading="busy" :disable="!file" @click="doImport" />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </q-page>
</template>

<script setup lang="ts">
/* The builds a script can run: the newest of each project in each
 * catalogue, fetched when used, and the ones saved to stay. */
import { onMounted, ref } from 'vue'
import { useQuasar } from 'quasar'
import { matDeleteOutline } from '@quasar/extras/material-icons'
import { useCatalog, type DeviceRow } from '../stores/catalog'
import { request, upload } from '../lib/front'

const catalog = useCatalog()
const quasar = useQuasar()
const importing = ref(false)
const file = ref<File | null>(null)
const busy = ref(false)
const refreshing = ref(false)
const saving = ref<string | null>(null)
const said = ref<string[]>([])

onMounted(() => { void catalog.refreshDevices() })

function when(stamp?: string) {
  if (!stamp || stamp.length < 12) return stamp ?? '—'
  return `${stamp.slice(0, 4)}-${stamp.slice(4, 6)}-${stamp.slice(6, 8)} ${stamp.slice(8, 10)}:${stamp.slice(10, 12)}`
}

function whence(d: DeviceRow) {
  if (d.source === 'compiled') return 'as last compiled in its own tree, run in place'
  const from = d.source === 'builds' ? 'builds/ beside SIMesh' : 'the web'
  return `${from}, ${d.fetched ? 'fetched' : 'fetched when used'}`
}

/** Whether the build a latest row stands for now is saved already. */
function saved(d: DeviceRow) {
  return !!d.stamp && catalog.saved.some(s => s.ref === `${d.project}_${d.catalogue}_${d.stamp}`)
}

async function save(d: DeviceRow) {
  saving.value = d.ref
  const r = await request('device_save', { ref: d.ref })
  saving.value = null
  if (!r.ok) { quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 8000 }); return }
  quasar.notify({ type: 'positive', message: `saved ${r.ref as string}`, timeout: 3000 })
  await catalog.refreshDevices()
}

function remove(d: DeviceRow) {
  quasar.dialog({ title: 'Delete a saved build', message: `Delete ${d.ref}?`, cancel: true, persistent: true })
    .onOk(() => {
      void (async () => {
        const r = await request('device_delete', { ref: d.ref })
        if (!r.ok) quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 8000 })
        await catalog.refreshDevices()
      })()
    })
}

async function doImport() {
  if (!file.value) return
  busy.value = true
  const r = await upload('/api/devices/import', file.value.name, file.value)
  busy.value = false
  if (!r.ok) { quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 8000 }); return }
  quasar.notify({ type: 'positive', message: `imported as ${r.ref as string}`, timeout: 3000 })
  importing.value = false
  file.value = null
  await catalog.refreshDevices()
}

async function refresh() {
  refreshing.value = true
  const r = await request('device_refresh')
  refreshing.value = false
  said.value = (r.said as string[] | undefined) ?? (r.ok ? [] : [r.error ?? 'refused'])
  await catalog.refreshDevices()
}
</script>

<style scoped>
.dp { overflow-y: auto; }
.dp-body { padding: 16px 20px 32px; max-width: 1100px; }
.dp-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.dp-second { margin-top: 28px; }
.dp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.dp-text { font-size: 12px; color: #9ca3af; line-height: 1.5; margin-bottom: 12px; max-width: 760px; }
.dp-text code { color: #cbd5e1; }
.dp-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.dp-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35; white-space: nowrap;
}
.dp-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.dp-act { text-align: right; width: 1%; white-space: nowrap; }
.dp-name { font-weight: 500; color: #e5e7eb; }
.dp-sub { font-size: 11px; color: #6b7280; }
.dp-bad { font-size: 11px; color: #fca5a5; }
.dp-off td { color: #6b7280; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
.dp-said { margin-top: 12px; font: 11px ui-monospace, monospace; color: #9ca3af; }
</style>
