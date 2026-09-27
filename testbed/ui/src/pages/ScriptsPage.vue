<template>
  <q-page class="sp">
    <div class="sp-list">
      <div class="sp-head">
        <span class="sp-heading">Scripts</span>
        <q-space />
        <q-btn flat dense no-caps size="sm" label="New…" @click="askNew" />
      </div>
      <q-list dense>
        <q-item v-for="s in catalog.scripts" :key="s.name" clickable :active="s.name === current"
                active-class="sp-active" @click="openScript(s.name)">
          <q-item-section>
            <q-item-label>{{ s.name }}</q-item-label>
            <q-item-label caption class="sp-doc">{{ s.error ?? s.doc }}</q-item-label>
          </q-item-section>
        </q-item>
      </q-list>
    </div>

    <div class="sp-main-pane">
      <template v-if="current">
        <q-toolbar class="sp-bar">
          <span class="sp-title">{{ current }}<span v-if="dirty" class="sp-dirty"> •</span></span>
          <q-btn flat dense no-caps label="Save" :disable="!dirty" @click="save" />
          <q-btn flat dense no-caps label="Save as…" @click="askSaveAs" />
          <q-space />
          <q-btn unelevated dense no-caps color="primary" label="Run…" :disable="!info?.main"
                 @click="running = true">
            <q-tooltip>{{ info?.main ? 'Run its main(sim) on a simulation' : 'It has no main(sim): its setup runs when a simulation is started with it' }}</q-tooltip>
          </q-btn>
        </q-toolbar>
        <div class="sp-split">
          <textarea v-model="text" class="sp-editor" spellcheck="false" @keydown.tab.prevent="indent" />
          <div v-if="shownRun && runOutput" class="sp-output">
            <div class="sp-output-head">
              <span>{{ runOutput.name }} on {{ runOutput.sim }}</span>
              <q-space />
              <q-btn v-if="runOutput.state === 'running'" flat dense no-caps size="sm" color="negative"
                     label="Stop" @click="stopRun(runOutput.run)" />
              <q-btn flat dense round size="sm" @click="shownRun = null">×</q-btn>
            </div>
            <pre ref="outputEl">{{ runOutput.lines.join('\n') }}</pre>
          </div>
        </div>
      </template>
      <div v-else class="sp-intro">
        <div class="sp-heading">Scripts are Python against the simesh library</div>
        <p>
          <code>async def setup(node)</code> runs inside the simulation, once per station
          with no state, after the node's declared name, role and radio figures and
          before its radio starts: <code>node.run(line)</code> in its own language,
          <code>node.set_role(…)</code>, <code>node.peer_tcp(other)</code>, its
          <code>tags</code>, <code>kind</code> and <code>name</code>. A simulation
          started with the script uses it.
        </p>
        <p>
          <code>async def main(sim)</code> drives a running simulation from a process
          of its own: <code>sim.nodes(tag=…)</code> chooses stations, and on them
          <code>.run(line)</code>, <code>.announce()</code>,
          <code>.message(to, text)</code>, <code>.set_radio(…)</code>, each spread over
          <code>spread=</code> seconds and held back <code>after=</code> seconds of the
          run's own clock; <code>sim.until(t)</code>, <code>sim.plan(…)</code>,
          <code>sim.snapshot(name)</code>. Run starts it here, with its output beside it.
          A simulation Run started for it is paused when main ends: its stations
          stopped with their state kept, to be resumed on the Simulations tab.
        </p>
        <p>
          <code>def report(run_dir)</code> runs after that and returns the run's
          report as Markdown, shown by its simulation's Report button on the
          Simulations tab.
        </p>
      </div>
    </div>

    <q-dialog v-model="running">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Run {{ current }}</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-btn-toggle v-model="runOn" dense no-caps unelevated toggle-color="primary"
                        :options="[{ label: 'on a running simulation', value: 'running' },
                                   { label: 'on a new one', value: 'new' }]" />
          <q-select v-if="runOn === 'running'" v-model="runSim" :options="runningNames" dense outlined
                    label="simulation" />
          <template v-else>
            <q-select v-model="runGeodata" :options="geodataNames" dense outlined label="geodata" />
            <q-select v-model="runNodeset" :options="nodesetNames" dense outlined label="nodeset" />
            <q-btn-toggle v-model="runTime" dense no-caps unelevated toggle-color="primary"
                          :options="[{ label: 'real', value: 'real' }, { label: 'max', value: 'max' }]" />
            <q-select v-model="runSetup" :options="setupNames" dense outlined clearable
                      label="setup script" :hint="info?.setup ? 'empty: this script\'s own setup'
                                                              : 'empty: declared settings only'" />
          </template>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Run" :loading="starting" :disable="!runReady" @click="run" />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </q-page>
</template>

<script setup lang="ts">
/* Scripts: each one's text, edited here and saved through the front, which
 * checks it parses; Run starts its main on a simulation as a process of the
 * front's, and its output comes back here as it is written. */
import { computed, nextTick, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import { useCatalog, type ScriptRow } from '../stores/catalog'
import { useSim } from '../stores/sim'
import { saveIfEditing, useNodes } from '../stores/nodes'
import { request } from '../lib/front'

const catalog = useCatalog()
const sim = useSim()
const nodes = useNodes()
const quasar = useQuasar()
const current = ref<string | null>(null)
const info = ref<ScriptRow | null>(null)
const text = ref('')
const saved = ref('')
const running = ref(false)
const starting = ref(false)
const runOn = ref<'running' | 'new'>('running')
const runSim = ref<string | null>(null)
const runGeodata = ref<string | null>(null)
const runNodeset = ref<string | null>(null)
const runTime = ref('real')
const runSetup = ref<string | null>(null)
const setupNames = computed(() => catalog.scripts.filter(s => s.setup && !s.error).map(s => s.name))
const shownRun = ref<string | null>(null)
const outputEl = ref<HTMLPreElement>()

const dirty = computed(() => text.value !== saved.value)
const runningNames = computed(() => sim.sims.filter(s => s.state === 'running').map(s => s.name))
const geodataNames = computed(() => catalog.geodata.filter(g => !g.error).map(g => g.name))
const nodesetNames = computed(() => catalog.nodesets.map(n => n.name))
const runReady = computed(() => (runOn.value === 'running' ? !!runSim.value : !!(runGeodata.value && runNodeset.value)))
const runOutput = computed(() => (shownRun.value ? catalog.runs[shownRun.value] ?? null : null))

watch(running, (open) => {
  if (!open) return
  runSim.value = runSim.value ?? sim.selected ?? runningNames.value[0] ?? null
  runGeodata.value = runGeodata.value ?? nodes.geodata
  runNodeset.value = runNodeset.value ?? nodes.nodeset?.name ?? null
  if (!runningNames.value.length) runOn.value = 'new'
})
watch(() => runOutput.value?.lines.length, async () => {
  await nextTick()
  const el = outputEl.value
  if (el) el.scrollTop = el.scrollHeight
})

function tell(error: string | null | undefined, done?: string) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 8000 })
  else if (done) quasar.notify({ type: 'positive', message: done, timeout: 2500 })
}

function guard(go: () => void) {
  if (!dirty.value) { go(); return }
  quasar.dialog({ title: 'Unsaved changes', message: `Discard the changes to ${current.value}?`,
                  cancel: true, persistent: true }).onOk(go)
}

function openScript(name: string) {
  guard(async () => {
    const r = await request('script_open', { name })
    if (!r.ok) { tell(r.error); return }
    current.value = name
    info.value = r.script as ScriptRow
    text.value = saved.value = r.text as string
  })
}

async function save() {
  const r = await request('script_save', { name: current.value, text: text.value })
  if (!r.ok) { tell(r.error); return }
  info.value = r.script as ScriptRow
  saved.value = text.value
  tell(null, 'saved')
  void catalog.refresh()
}

function askName(title: string, then: (name: string) => void) {
  quasar.dialog({ title, message: 'Lower-case letters, digits and hyphens.',
                  prompt: { model: '', type: 'text' }, cancel: true })
    .onOk((name: string) => { if (name.trim()) then(name.trim()) })
}

function askNew() {
  guard(() => askName('New script', async (name) => {
    const r = await request('script_new', { name })
    if (!r.ok) { tell(r.error); return }
    current.value = name
    info.value = r.script as ScriptRow
    text.value = saved.value = r.text as string
    void catalog.refresh()
  }))
}

function askSaveAs() {
  askName('Save script as', async (name) => {
    const r = await request('script_save_as', { name, text: text.value })
    if (!r.ok) { tell(r.error); return }
    current.value = name
    info.value = r.script as ScriptRow
    saved.value = text.value
    void catalog.refresh()
  })
}

async function run() {
  if (dirty.value) await save()
  if (runOn.value === 'new') {
    const error = await saveIfEditing(runNodeset.value)
    if (error) { tell(error); return }
  }
  starting.value = true
  const r = await request('script_run', runOn.value === 'running'
    ? { name: current.value, sim: runSim.value }
    : { name: current.value, geodata: runGeodata.value, nodeset: runNodeset.value, time: runTime.value,
        ...(runSetup.value ? { setup: runSetup.value } : {}) })
  starting.value = false
  if (!r.ok) { tell(r.error); return }
  running.value = false
  shownRun.value = r.run as string
}

async function stopRun(id: string) {
  const r = await request('script_stop', { run: id })
  if (!r.ok) tell(r.error)
}

/** Tab in the editor puts four spaces in, as Python wants. */
function indent(event: KeyboardEvent) {
  const el = event.target as HTMLTextAreaElement
  const start = el.selectionStart, end = el.selectionEnd
  text.value = `${text.value.slice(0, start)}    ${text.value.slice(end)}`
  void nextTick(() => { el.selectionStart = el.selectionEnd = start + 4 })
}
</script>

<style scoped>
.sp { display: flex; height: 100%; }
.sp-list { width: 280px; flex: none; border-right: 1px solid #262c35; overflow-y: auto; padding: 10px 0; }
.sp-head { display: flex; align-items: center; padding: 0 12px 4px; }
.sp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.sp-doc { font-size: 11px; color: #6b7280 !important; }
.sp-active { background: #1a2029; color: #fff; }
.sp-main-pane { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; }
.sp-bar { min-height: 38px; gap: 6px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.sp-title { font-size: 14px; font-weight: 500; padding: 0 8px; }
.sp-dirty { color: #f59e0b; }
.sp-split { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }
.sp-editor {
  flex: 1 1 60%; min-height: 0; resize: none; border: none; outline: none; padding: 10px 14px;
  background: #0e1116; color: #d1d5db; font: 13px/1.5 ui-monospace, monospace; tab-size: 4;
}
.sp-output { flex: 1 1 40%; min-height: 0; display: flex; flex-direction: column; border-top: 1px solid #262c35; }
.sp-output-head { display: flex; align-items: center; padding: 2px 8px 2px 12px; font-size: 12px; color: #9ca3af; }
.sp-output pre {
  flex: 1 1 auto; margin: 0; overflow: auto; padding: 6px 12px; background: #0b0d10;
  font: 12px/1.4 ui-monospace, monospace; color: #c7ccd4; white-space: pre-wrap; word-break: break-word;
}
.sp-intro { padding: 16px 20px; max-width: 720px; font-size: 13px; color: #9ca3af; line-height: 1.6; }
</style>
