<template>
  <q-page class="sims-page">
    <div class="sims-body">
      <div class="sims-head">
        <div class="sims-heading">Simulations</div>
        <q-space />
        <q-btn unelevated dense no-caps color="primary" label="Run from current world"
               :disable="!currentReady" @click="runCurrent">
          <q-tooltip>{{ currentReason }}</q-tooltip>
        </q-btn>
        <q-select v-model="currentScript" :options="setupScripts" dense outlined options-dense clearable
                  class="sims-script" label="setup script (optional)" />
      </div>

      <div v-if="!sim.sims.length" class="sims-none">
        No simulations yet. Run from current world starts the Nodes tab's nodeset on its
        geodata in real time, for working with the stations by hand; a script's Run
        starts one too, and so does
        <code>simesh new --geodata &lt;g&gt; --nodeset &lt;n&gt; [--script &lt;s&gt;]</code>
        from a shell.
      </div>

      <table v-else class="sims-table">
        <thead>
          <tr>
            <th>Simulation</th>
            <th>Nodeset</th>
            <th>Time</th>
            <th>Plan</th>
            <th>Finish</th>
            <th class="num">Stations</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <template v-for="s in sim.sims" :key="`${s.state === 'ended' ? 'run' : 'sim'}:${s.name}`">
            <tr :class="{ 'sims-selected': s.name === sim.selected, 'sims-exited': s.state === 'exited',
                          'sims-open': s.state === 'running' }"
                @click="s.state === 'running' && sim.attach(s.name)">
              <td>
                <span :class="s.state === 'running' ? 'sims-name' : 'sims-name-off'">{{ s.name }}</span>
                <div class="sims-state" :class="`sims-state-${s.state}`">
                  {{ s.state === 'exited' ? `exited (${s.code})` : s.state }}
                </div>
              </td>
              <td>
                {{ s.nodeset ?? '—' }}<span v-if="s.dirty" class="sims-dirty"> •</span>
                <div class="sims-sub">
                  on {{ s.geodata ?? '—' }}<template v-if="s.script">, set up by {{ s.script }}</template><template
                    v-if="s.snapshot">, from snapshot {{ s.snapshot }}</template>
                </div>
                <div class="sims-sub" :title="`control 127.0.0.1:${s.port}, ether ${s.ether}, run ${s.run}`">
                  {{ s.net }}
                </div>
                <div v-if="sim.progress[s.name]?.running" class="sims-sub sims-losses">
                  loss table {{ sim.progress[s.name]!.band }}: {{ sim.progress[s.name]!.done }}/{{ sim.progress[s.name]!.total }}
                </div>
              </td>
              <td class="mono">
                <div v-if="simText(s)">{{ simText(s) }} <span class="sims-sub">simulated</span></div>
                <div v-if="realText(s)">{{ realText(s) }} <span class="sims-sub">real</span></div>
                <div v-if="speedText(s)" class="sims-sub">{{ speedText(s) }}</div>
                <span v-if="!simText(s) && !realText(s)" class="sims-sub">—</span>
              </td>
              <td>
                <template v-if="phaseText(s)">
                  <div>{{ phaseText(s) }}</div>
                  <div class="sims-phases">
                    <span v-for="(p, i) in s.plan ?? []" :key="i" class="sims-phase"
                          :class="{ 'sims-phase-now': s.phase?.index === i,
                                    'sims-phase-past': s.phase ? i < s.phase.index : s.done }"
                          :style="{ flexGrow: phaseWeight(s, i) }"
                          :title="p.name">
                      <span v-if="s.phase?.index === i" class="sims-phase-fill"
                            :style="{ width: `${phaseProgress(s) * 100}%` }" />
                    </span>
                  </div>
                </template>
                <span v-else class="sims-sub">{{ s.state === 'ended' || s.state === 'paused' ? '—' : 'no plan' }}</span>
              </td>
              <td>
                <template v-if="etaText(s)">
                  {{ etaText(s) }}
                  <div v-if="s.phase?.eta" class="sims-sub">
                    {{ s.phase.name }} ends {{ clockTime(s.phase.eta) }}
                  </div>
                </template>
                <span v-else-if="s.state === 'paused'" class="sims-sub">
                  paused {{ s.paused_at ? clockTime(Date.parse(s.paused_at) / 1000) : '' }}
                </span>
                <span v-else-if="s.state === 'ended'" class="sims-sub">
                  started {{ s.started_at ? clockTime(Date.parse(s.started_at) / 1000) : '—' }}
                </span>
                <span v-else class="sims-sub">{{ s.plan && !s.done ? 'pace not known yet' : '—' }}</span>
              </td>
              <td class="num mono">
                <template v-if="s.state === 'paused' || s.state === 'ended'">{{ s.stations }}</template>
                <template v-else>
                  <span :class="{ 'sims-all-up': s.stations && up(s) === s.stations }">
                    {{ up(s) }}/{{ s.stations }}
                  </span>
                  <div class="sims-sub">{{ others(s) }}</div>
                </template>
              </td>
              <td class="sims-actions" @click.stop>
                <q-btn v-if="s.report" flat dense no-caps size="sm" label="Report" @click="reportOf = s.run" />
                <q-btn v-if="s.state === 'paused'" flat dense no-caps size="sm" color="primary"
                       label="Resume" @click="sim.resumeSim(s.name)">
                  <q-tooltip>Start it again as it ended, in a new run directory</q-tooltip>
                </q-btn>
                <q-btn v-if="s.state === 'running'" flat dense round size="sm" aria-label="More">
                  <span class="sims-more">⋯</span>
                  <q-menu auto-close anchor="bottom right" self="top right">
                    <q-list dense style="min-width: 210px">
                      <q-item clickable @click="sim.pauseSim(s.name)">
                        <q-item-section>Pause</q-item-section>
                        <q-item-section side><span class="sims-sub">stop, keep state</span></q-item-section>
                      </q-item>
                      <q-separator />
                      <q-item clickable @click="to(s.name, 'reset_all')">
                        <q-item-section>Reset all</q-item-section>
                        <q-item-section side><span class="sims-sub">restart</span></q-item-section>
                      </q-item>
                      <q-item clickable @click="askFactory(s.name)">
                        <q-item-section>Factory reset all</q-item-section>
                        <q-item-section side><span class="sims-sub">wipe state</span></q-item-section>
                      </q-item>
                      <q-separator />
                      <q-item clickable @click="askSnapshot(s.name)"><q-item-section>Save snapshot as…</q-item-section></q-item>
                      <q-item clickable :disable="!catalog.snapshots.length" @click="restoring = s.name">
                        <q-item-section>Load snapshot into it…</q-item-section>
                      </q-item>
                    </q-list>
                  </q-menu>
                </q-btn>
                <q-btn v-if="s.state === 'paused' || s.state === 'ended'" flat dense round size="sm"
                       :icon="matDeleteOutline" color="grey-6" aria-label="Delete" @click="askDelete(s)">
                  <q-tooltip>Delete the run: its directory, logs, report{{ s.state === 'paused' ? ' and saved state' : '' }}</q-tooltip>
                </q-btn>
                <q-btn v-else flat dense no-caps size="sm" color="negative"
                       :label="s.state === 'exited' ? 'Remove' : 'Stop'"
                       :disable="s.state === 'stopping'" @click="askStop(s.name, s.state)" />
              </td>
            </tr>
            <tr v-if="s.state === 'exited' && s.tail.length" class="sims-tail-row">
              <td colspan="7"><pre class="sims-tail">{{ s.tail.join('\n') }}</pre></td>
            </tr>
            <tr v-else-if="s.errors.length" class="sims-tail-row">
              <td colspan="7" class="sims-errors">
                <div v-for="([at, text], i) in s.errors" :key="i">
                  <span class="mono">{{ clockTime(at) }}</span> {{ text }}
                </div>
              </td>
            </tr>
          </template>
        </tbody>
      </table>

      <div class="sims-sub q-mt-lg">
        Each simulation is its own simd with its own ether, stations, network and
        run directory under <code>testbed/runs/</code>. A station's web UI is at
        <code>http://&lt;station&gt;.&lt;simulation&gt;.sim.localhost:{{ socket.port }}/</code>.
        A new simulation's loss tables come from the cache, or are computed
        through the planner before it starts, with progress in its row.
      </div>
    </div>

    <!-- Load a snapshot into a running simulation. -->
    <q-dialog v-model="restoringOpen">
      <q-card style="min-width: 360px">
        <q-card-section class="text-subtitle2">Load a snapshot into {{ restoring }}</q-card-section>
        <q-card-section class="text-caption text-grey-6 q-pt-none">
          The network as it was: its nodeset, script and loss table, and every
          station's identity, keys, paths and message history. What the
          simulation is running now goes.
        </q-card-section>
        <q-list dense bordered separator style="max-height: 50vh; overflow-y: auto">
          <q-item v-for="s in catalog.snapshots" :key="s.name" clickable v-close-popup
                  @click="to(restoring!, 'snapshot_load', { name: s.name })">
            <q-item-section>{{ s.name }}</q-item-section>
            <q-item-section side><span class="text-caption text-grey-6">{{ s.nodeset ?? '' }}</span></q-item-section>
          </q-item>
        </q-list>
        <q-card-actions align="right"><q-btn flat no-caps label="Cancel" v-close-popup /></q-card-actions>
      </q-card>
    </q-dialog>

    <ReportDialog :run="reportOf" @close="reportOf = null" />
  </q-page>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import { useSim, type SimSummary } from '../stores/sim'
import { useCatalog } from '../stores/catalog'
import { saveIfEditing, useNodes } from '../stores/nodes'
import { useSocket } from '../stores/socket'
import { clockTime, etaText, phaseText, realText, simText, speedText } from '../components/runtime'
import ReportDialog from '../components/ReportDialog.vue'
import { matDeleteOutline } from '@quasar/extras/material-icons'

const sim = useSim()
const catalog = useCatalog()
const nodes = useNodes()
const socket = useSocket()
const quasar = useQuasar()
const currentScript = ref<string | null>(null)
/** The run whose report is on show. */
const reportOf = ref<string | null>(null)
/** The simulation a snapshot is being chosen for, while the dialog is open. */
const restoring = ref<string | null>(null)
const restoringOpen = computed<boolean>({
  get: () => restoring.value !== null,
  set: (v) => { if (!v) restoring.value = null },
})

const setupScripts = computed(() => catalog.scripts.filter(s => s.setup !== false && !s.error).map(s => s.name))

/* Run from current world: the Nodes tab's nodeset on its geodata, its
 * changes saved first, since a simulation runs the file. */
const currentReason = computed(() => {
  if (!nodes.nodeset) return 'Open a nodeset in the Nodes tab first'
  if (!nodes.nodeset.name) return 'Save the Nodes tab\'s nodeset first: it has no name yet'
  if (!nodes.geodata) return 'Choose the geodata on the Geodata tab first'
  return `${nodes.nodeset.name}${nodes.nodeset.dirty ? ' (saved first)' : ''} on ${nodes.geodata}, in real time`
})
const currentReady = computed(() => !!(nodes.nodeset?.name && nodes.geodata))

async function runCurrent() {
  if (!currentReady.value) return
  const error = await saveIfEditing(nodes.nodeset!.name)
  if (error) { quasar.notify({ type: 'negative', message: error, timeout: 6000 }); return }
  sim.newSim({ geodata: nodes.geodata!, nodeset: nodes.nodeset!.name!, time: 'real',
               ...(currentScript.value ? { script: currentScript.value } : {}) })
}

onMounted(() => { void catalog.refresh() })

/** One of a simulation's own verbs, sent to it through the front. */
function to(simName: string, type: string, fields: Record<string, unknown> = {}) {
  socket.send({ sim: simName, type, ...fields })
}

function askFactory(simName: string) {
  quasar.dialog({
    title: `Factory reset ${simName}`,
    message: 'Wipe every station\'s state and set it up again: identities, keys, paths and '
           + 'message history go.',
    cancel: true, persistent: true,
  }).onOk(() => to(simName, 'factory_reset_all'))
}

function askSnapshot(simName: string) {
  quasar.dialog({
    title: `Save a snapshot of ${simName}`, message: 'Lower-case letters, digits and hyphens.',
    prompt: { model: '', type: 'text' }, cancel: true,
  }).onOk((n: string) => { if (n.trim()) to(simName, 'snapshot_save_as', { name: n.trim() }) })
}

function up(s: SimSummary) { return s.counts.up ?? 0 }

/** The stations that are not up, by what they are doing instead. */
function others(s: SimSummary) {
  return Object.entries(s.counts)
    .filter(([status, n]) => status !== 'up' && n)
    .map(([status, n]) => `${n} ${status}`).join(', ')
}

/** A phase's share of the plan's bar: its length in T. */
function phaseWeight(s: SimSummary, i: number) {
  const plan = s.plan ?? []
  const from = i === 0 ? (s.plan_from ?? plan[0]!.until) : plan[i - 1]!.until
  return Math.max(1, plan[i]!.until - from)
}

function phaseProgress(s: SimSummary) {
  if (!s.phase || s.t === null) return 0
  const { from, until } = s.phase
  return Math.min(1, Math.max(0, (s.t - from) / Math.max(1, until - from)))
}

function askDelete(s: SimSummary) {
  const run = s.run.split('/').pop()!
  quasar.dialog({
    title: `Delete ${run}`,
    message: `Its run directory goes, with its logs, record and report${s.state === 'paused'
      ? ', and the state it was paused with: it can no longer be resumed' : ''}.`,
    cancel: true, persistent: true,
  }).onOk(() => sim.deleteRun(run))
}

function askStop(simName: string, state: string) {
  if (state === 'exited') { sim.stopSim(simName); return }
  quasar.dialog({
    title: `Stop ${simName}`,
    message: 'Its stations are flushed and stopped and its simd ends. The run '
           + 'directory stays; nothing is saved to the nodeset or as a snapshot, and it cannot '
           + 'be resumed (⋯ ▸ Pause keeps it resumable).',
    cancel: true,
    persistent: true,
  }).onOk(() => sim.stopSim(simName))
}
</script>

<style scoped>
.sims-page { overflow-y: auto; }
.sims-body { padding: 16px 20px 32px; max-width: 1200px; }
.sims-head { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }
.sims-heading { font-size: 14px; font-weight: 500; color: #d1d5db; margin-bottom: 10px; }
.sims-head .sims-heading { margin-bottom: 0; }
.sims-script { min-width: 200px; }
.sims-none { color: #6b7280; font-size: 13px; line-height: 1.5; max-width: 760px; }
.sims-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.sims-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35; white-space: nowrap;
}
.sims-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.sims-table .num { text-align: right; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
.sims-selected td { background: #1a2029; }
.sims-exited td { color: #9ca3af; }
.sims-name { color: #7dd3fc; font-weight: 500; }
.sims-open { cursor: pointer; }
.sims-open:hover td { background: #1b2028; }
.sims-state-paused { color: #a78bfa; }
.sims-state-ended { color: #6b7280; }
.sims-name-off { font-weight: 500; }
.sims-state { font-size: 11px; color: #6b7280; }
.sims-state-running { color: #22c55e; }
.sims-state-starting, .sims-state-stopping { color: #f59e0b; }
.sims-state-exited { color: #ef4444; }
.sims-sub { font-size: 11px; color: #6b7280; }
.sims-dirty { color: #f59e0b; }
.sims-all-up { color: #22c55e; }
.sims-phases { display: flex; gap: 2px; margin-top: 4px; width: 180px; height: 5px; }
.sims-phase { position: relative; flex-basis: 0; background: #2b313b; border-radius: 2px; overflow: hidden; }
.sims-phase-past { background: #3b82f6; }
.sims-phase-now { background: #1e3a5f; }
.sims-phase-fill { position: absolute; left: 0; top: 0; bottom: 0; background: #3b82f6; }
.sims-actions { white-space: nowrap; text-align: right; }
.sims-tail-row td { padding-top: 0; }
.sims-tail {
  margin: 0; padding: 6px 8px; max-height: 180px; overflow: auto;
  font: 11px ui-monospace, monospace; color: #c7ccd4; background: #0e1116; border-radius: 3px;
  white-space: pre-wrap; word-break: break-word;
}
.sims-errors { font-size: 12px; color: #fca5a5; }
.sims-more { font-size: 16px; line-height: 1; color: #9ca3af; }
.sims-losses { color: #fbbf24; }
</style>
