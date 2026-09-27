<template>
  <q-page class="dp">
    <div class="dp-body">
      <div class="dp-head">
        <div class="dp-heading">Devices</div>
        <q-space />
        <q-btn flat dense no-caps label="Refresh" :loading="refreshing" @click="refresh">
          <q-tooltip>Fetch the newest devices for this machine from the web and from builds/ beside SIMesh</q-tooltip>
        </q-btn>
        <q-btn unelevated dense no-caps color="primary" label="Import…" @click="importing = true" />
      </div>
      <div class="dp-text">
        The station builds a node can run, the newest three of each catalogue.
        A node names one by catalogue (the newest there) or by the device
        itself. This machine is {{ catalog.arch || '…' }}.
      </div>

      <table class="dp-table">
        <thead>
          <tr><th>Device</th><th>Plays</th><th>Kind</th><th>From</th><th>Built</th><th>Named by</th></tr>
        </thead>
        <tbody>
          <tr v-for="d in catalog.devices" :key="d.ref" :class="{ 'dp-off': !d.runs_here || d.error }">
            <td>
              <div class="dp-name">{{ d.name }}</div>
              <div class="dp-sub mono">{{ d.ref }}</div>
              <div v-if="d.error" class="dp-bad">{{ d.error }}</div>
              <div v-else-if="!d.runs_here" class="dp-sub">built for {{ d.arch }}: not this machine</div>
            </td>
            <td>{{ d.stands_for ? `virtual ${d.stands_for}` : '—' }}</td>
            <td class="mono">{{ d.kind ?? '—' }}</td>
            <td>{{ d.local ? 'local build' : d.catalogue }}</td>
            <td class="mono">{{ when(d.stamp) }}</td>
            <td>
              <span v-if="d.newest_of" class="dp-alias">{{ d.newest_of }}</span>
              <span class="dp-sub mono">{{ d.local ? d.ref : '' }}</span>
            </td>
          </tr>
          <tr v-if="!catalog.devices.length">
            <td colspan="6" class="dp-sub">No devices yet: Refresh catalogues, or Import a device file.</td>
          </tr>
        </tbody>
      </table>
      <div v-if="said.length" class="dp-said">
        <div v-for="(line, i) in said" :key="i">{{ line }}</div>
      </div>
    </div>

    <q-dialog v-model="importing">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Import a device file</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-file v-model="file" dense outlined label="device zip" accept=".zip,application/zip" />
          <div class="text-caption text-grey-6">
            A zip with a node.yaml at its top, built for this machine. It joins
            the catalogue <code>imported</code>.
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
/* The devices there are: fetched from the catalogues, imported here, or
 * local builds described under devices/local/. */
import { onMounted, ref } from 'vue'
import { useQuasar } from 'quasar'
import { useCatalog } from '../stores/catalog'
import { request, upload } from '../lib/front'

const catalog = useCatalog()
const quasar = useQuasar()
const importing = ref(false)
const file = ref<File | null>(null)
const busy = ref(false)
const refreshing = ref(false)
const said = ref<string[]>([])

onMounted(() => { void catalog.refreshDevices() })

function when(stamp?: string) {
  if (!stamp || stamp.length < 12) return stamp ?? '—'
  return `${stamp.slice(0, 4)}-${stamp.slice(4, 6)}-${stamp.slice(6, 8)} ${stamp.slice(8, 10)}:${stamp.slice(10, 12)}`
}

async function doImport() {
  if (!file.value) return
  busy.value = true
  const r = await upload('/api/devices/import', file.value.name, file.value)
  busy.value = false
  if (!r.ok) { quasar.notify({ type: 'negative', message: r.error ?? 'refused', timeout: 8000 }); return }
  quasar.notify({ type: 'positive', message: `imported ${r.name as string}`, timeout: 3000 })
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
.dp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.dp-text { font-size: 12px; color: #9ca3af; line-height: 1.5; margin-bottom: 12px; max-width: 760px; }
.dp-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.dp-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35; white-space: nowrap;
}
.dp-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.dp-name { font-weight: 500; color: #e5e7eb; }
.dp-sub { font-size: 11px; color: #6b7280; }
.dp-bad { font-size: 11px; color: #fca5a5; }
.dp-off td { color: #6b7280; }
.dp-alias { background: #1e3a5f; color: #7dd3fc; border-radius: 3px; padding: 1px 6px; font-size: 12px; margin-right: 6px; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
.dp-said { margin-top: 12px; font: 11px ui-monospace, monospace; color: #9ca3af; }
</style>
