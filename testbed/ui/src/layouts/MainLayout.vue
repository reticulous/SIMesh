<template>
  <q-layout view="hHh lpR fFf" class="sim-layout">
    <q-header class="sim-header">
      <div class="sim-tabs-row">
        <q-tabs v-model="tab" dense no-caps inline-label align="left" class="sim-tabs"
                active-color="white" indicator-color="primary">
          <template v-if="socket.front">
            <q-tab name="devices" label="Firmware" />
            <q-tab name="antennas" label="Antennas" />
            <q-tab name="geodata" label="Geodata" />
          </template>
          <q-tab name="nodes" label="Nodes" />
          <template v-if="socket.front">
            <q-tab name="scripts" label="Scripts" />
            <!-- On it with a simulation open, the tab goes back to the list. On
                 mousedown, before the tab changes: from another tab it leaves
                 the open simulation open. -->
            <q-tab name="sims" label="Simulations" @mousedown="sim.view === 'sims' && sim.detach()" />
          </template>
        </q-tabs>
        <span class="sim-link" :class="{ 'sim-link-off': !socket.connected }">
          {{ socket.connected ? 'connected' : 'reconnecting…' }}
        </span>
      </div>
    </q-header>

    <q-page-container class="sim-page-container">
      <!-- Each kept mounted while another is on show, so a console left open,
           a nodeset half-built or a script half-written is still there on the
           way back. -->
      <DevicesPage v-if="socket.front" v-show="sim.view === 'devices'" />
      <AntennasPage v-if="socket.front" v-show="sim.view === 'antennas'" />
      <GeodataPage v-if="socket.front" v-show="sim.view === 'geodata'" />
      <!-- One map for both: the nodeset being edited on the Nodes tab, and an
           open simulation's live map on the Simulations tab, in place of its list. -->
      <NodesPage v-show="sim.view === mapTab" />
      <ScriptsPage v-if="socket.front" v-show="sim.view === 'scripts'" />
      <SimsPage v-if="socket.front && sim.view === 'sims' && !sim.attached" />
    </q-page-container>

    <q-footer class="sim-status">
      <span v-if="socket.front">
        <b :class="{ 'sim-status-on': sim.runningSims }">{{ sim.runningSims }}</b>
        simulation{{ sim.runningSims === 1 ? '' : 's' }} running<template
          v-for="s in runningSims" :key="s.name"><span class="sim-status-sim" @click="sim.attach(s.name)"
          :title="statusTitle(s)">{{ s.name }}<span v-if="s.phase" class="sim-status-dim"> {{ s.phase.name }}</span></span></template>
      </span>
      <span v-if="scriptsRunning" class="sim-status-scripts">
        · {{ scriptsRunning }} script{{ scriptsRunning === 1 ? '' : 's' }} running
      </span>
    </q-footer>
  </q-layout>
</template>

<script setup lang="ts">
/* The page: Firmware, Antennas, Geodata, Nodes, Scripts and Simulations, and a status
 * line under them all. Served by a simd on its own there is one simulation
 * and only the Nodes tab, attached to it. */
import { computed, watch } from 'vue'
import { useQuasar } from 'quasar'
import { useSim, type SimSummary, type Tab } from '../stores/sim'
import { useSocket } from '../stores/socket'
import { useCatalog } from '../stores/catalog'
import DevicesPage from '../pages/DevicesPage.vue'
import AntennasPage from '../pages/AntennasPage.vue'
import GeodataPage from '../pages/GeodataPage.vue'
import NodesPage from '../pages/NodesPage.vue'
import ScriptsPage from '../pages/ScriptsPage.vue'
import SimsPage from '../pages/SimsPage.vue'
import { etaText, paceText, phaseText } from '../components/runtime'
import { whenSaved } from '../lib/unsaved'

const sim = useSim()
const socket = useSocket()
const catalog = useCatalog()
const quasar = useQuasar()

const tab = computed<Tab>({
  get: () => sim.view,
  set: (v) => {
    if (sim.view === 'nodes' && v !== 'nodes') whenSaved(quasar, () => sim.show(v))
    else sim.show(v)
  },
})

const mapTab = computed(() => (socket.front && sim.attached ? 'sims' : 'nodes'))
const runningSims = computed(() => sim.sims.filter(s => s.state === 'running'))
const scriptsRunning = computed(() => catalog.runList.filter(r => r.state === 'running').length)

function statusTitle(s: SimSummary) {
  return [paceText(s), phaseText(s), etaText(s)].filter(Boolean).join(' · ')
}

socket.connect()

// With the front, the page starts where work starts: choosing a geodata.
watch(() => socket.front, (front, was) => {
  if (front && !was && sim.view === 'nodes' && !sim.attached) sim.show('geodata')
}, { immediate: true })

// simd and the front report what they could not do; the page says so and moves on.
watch(() => sim.errors.length, () => {
  const text = sim.errors.pop()
  if (text) quasar.notify({ type: 'negative', message: text, timeout: 6000 })
})
watch(() => sim.notices.length, () => {
  const text = sim.notices.pop()
  if (text) quasar.notify({ type: 'info', message: text, timeout: 6000 })
})
</script>

<style scoped>
.sim-header { background: #171b21; box-shadow: none; border-bottom: 1px solid #262c35; }
.sim-tabs-row { display: flex; align-items: center; }
.sim-tabs { flex: 1 1 auto; min-width: 0; min-height: 32px; }
.sim-link { font: 11px ui-monospace, monospace; color: #22c55e; padding-right: 8px; }
.sim-link-off { color: #f59e0b; }
.sim-page-container { height: 100vh; }
.sim-status {
  display: flex; align-items: center; gap: 6px; min-height: 22px; padding: 0 10px;
  background: #171b21; border-top: 1px solid #262c35; font-size: 11px; color: #9ca3af;
}
.sim-status-on { color: #22c55e; }
.sim-status-sim { margin-left: 8px; color: #7dd3fc; cursor: pointer; }
.sim-status-sim:hover { text-decoration: underline; }
.sim-status-dim { color: #6b7280; }
.sim-status-scripts { color: #9ca3af; }
</style>
