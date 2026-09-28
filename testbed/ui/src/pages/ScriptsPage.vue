<template>
  <q-page class="sp">
    <div class="sp-list">
      <div class="sp-head">
        <span class="sp-heading">Scripts</span>
        <q-space />
        <q-btn flat dense no-caps size="sm" label="New…" @click="askNew" />
      </div>
      <q-list dense>
        <q-item v-for="s in runnable" :key="s.name" clickable :active="s.name === current"
                active-class="sp-active" @click="openScript(s.name)">
          <q-item-section>
            <q-item-label>{{ s.name }}</q-item-label>
            <q-item-label caption class="sp-doc">{{ s.error ?? s.doc }}</q-item-label>
          </q-item-section>
        </q-item>
      </q-list>
      <!-- Scripts others include: parts of those, opened here to edit, never run alone. -->
      <template v-if="included.length">
        <div class="sp-head sp-subhead"><span class="sp-heading">Included by scripts</span></div>
        <q-list dense>
          <q-item v-for="s in included" :key="s.name" clickable :active="s.name === current"
                  active-class="sp-active" @click="openScript(s.name)">
            <q-item-section>
              <q-item-label>{{ s.name }}</q-item-label>
              <q-item-label caption class="sp-doc">{{ s.error ?? `part of ${s.included_by!.join(', ')}` }}</q-item-label>
            </q-item-section>
          </q-item>
        </q-list>
      </template>
    </div>

    <div class="sp-main-pane">
      <template v-if="current || setup">
        <q-toolbar class="sp-bar">
          <span class="sp-title">{{ current ?? `${setup} setup` }}<span v-if="anyDirty" class="sp-dirty"> •</span></span>
          <q-btn flat dense no-caps label="Save" :disable="!tab || tab.readonly || (!tabDirty(tab) && !tab.fresh)"
                 @click="save" />
          <q-btn v-if="!setup" flat dense no-caps label="Save as…" :disable="!tab || tab.readonly || !!tab.nodeset"
                 @click="askSaveAs" />
          <q-space />
          <q-btn v-if="!setup" unelevated dense no-caps color="primary" label="Run…"
                 :disable="!info || !!info.error || !!includedBy.length" @click="running = true">
            <q-tooltip>{{ includedBy.length
              ? `Part of ${includedBy.join(', ')}, which include it: run one of those`
              : 'Run it from its top: a new simulation of its own, or on a running one' }}</q-tooltip>
          </q-btn>
        </q-toolbar>
        <!-- The script, every file it imports (another script to edit, or the
             library's own module to read), scripts/globals.py, and the setup
             file of each nodeset it would run on. Or one nodeset's setup file alone. -->
        <q-tabs v-model="tabKey" dense no-caps inline-label align="left" class="sp-tabs"
                active-color="white" indicator-color="primary">
          <q-tab v-for="t in tabs" :key="t.key" :name="t.key">
            <span>{{ t.label }}<span v-if="tabDirty(t)" class="sp-dirty"> •</span></span>
            <span v-if="t.readonly" class="sp-ro">library</span>
          </q-tab>
        </q-tabs>
        <div class="sp-split">
          <textarea v-if="tab" v-model="tab.text" class="sp-editor" spellcheck="false" :readonly="tab.readonly"
                    :class="{ 'sp-readonly': tab.readonly }" @keydown.tab.prevent="indent" />
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
          A script runs from its top to its end, each call doing what it says:
          <code>from simesh import *</code>, then its declarations,
          <code>time("real")</code> (or <code>"max"</code>, or a pace such as
          <code>10</code>), <code>firmware(which, "reticulous_dev_latest")</code> and
          <code>on_first_boot(which, lines)</code>, then what it does:
          <code>up(which)</code>, <code>exec(which, lines, pause=…)</code>,
          <code>announce(which)</code>, <code>send_msg(a, b, text)</code>,
          <code>max_tx_pwr(which)</code>, <code>wait(s)</code>,
          <code>snapshot(name)</code>, <code>pause()</code>. <code>which</code> is
          <code>"all"</code>, a name, a list, or <code>nodes(tag=…)</code>, combined
          with <code>&amp; | - ~</code>.
        </p>
        <p>
          A script's declarations start with <code>include("scripts/startup.py")</code>,
          which tells every node its <code>radio(…)</code> and, for a node tagged with
          one, its <code>role(…)</code> in <code>on_first_boot</code>, then runs each
          nodeset's own <code>nodesets/&lt;name&gt;.py</code>, then starts the radios
          with <code>radio_up()</code>. The radio itself, the
          one the map draws, is in <code>scripts/globals.py</code>; a node tagged
          <code>no-radio</code> has none.
        </p>
        <p>
          Run starts its simulation on the Nodes tab's geodata and nodesets and
          goes over to it; it runs on when the script ends, unless the script
          paused it. <code>def report(run_dir)</code> returns the run's report as
          Markdown.
        </p>
      </div>
    </div>

    <q-dialog v-model="running">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Run {{ current }}</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-btn-toggle v-model="runOn" dense no-caps unelevated toggle-color="primary"
                        :options="[{ label: 'a new simulation', value: 'new' },
                                   { label: 'on a running one', value: 'running' }]" />
          <q-select v-if="runOn === 'running'" v-model="runSim" :options="runningNames" dense outlined
                    label="simulation" />
          <div v-else class="sp-world">
            <div><span>geodata</span><b>{{ nodes.geodata ?? 'none: choose it on the Geodata tab' }}</b></div>
            <div><span>{{ runLayers.length > 1 ? 'nodesets, merged' : 'nodeset' }}</span>
              <b>{{ runLayers.length ? runLayers.join(', ') : 'none: show a layer on the Nodes tab' }}</b></div>
            <div class="sp-world-note">What the Nodes tab shows; change it there. Real or virtual time is the script's own <code>time(…)</code> line.</div>
          </div>
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
/* Scripts: each one's text and the files it imports, in tabs, edited here
 * and saved through the front, which checks a script parses; Run starts it
 * as a process of the front's, and its output comes back here as it is
 * written. With `?setup=<nodeset>` the page is that nodeset's setup file
 * alone, which the Nodes tab's Edit setup opens. */
import { computed, nextTick, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import { useRoute, useRouter } from 'vue-router'
import { useCatalog, type ScriptRow } from '../stores/catalog'
import { useSim } from '../stores/sim'
import { saveIfEditing, useNodes } from '../stores/nodes'
import { request } from '../lib/front'

/** One tab: a script of the store (editable), a nodeset's setup file
 *  (editable, saved with nodeset_setup_save), or a library module
 *  (read-only). `fresh`: a setup file not written yet, its template, which
 *  Save writes. */
interface Tab {
  key: string; label: string; script: string | null; nodeset: string | null
  readonly: boolean; fresh: boolean; text: string; saved: string
}

const GLOBALS_PATH = 'scripts/globals.py'

const catalog = useCatalog()
const sim = useSim()
const nodes = useNodes()
const quasar = useQuasar()
const route = useRoute()
const router = useRouter()
const current = ref<string | null>(null)
/** The nodeset whose setup file the page shows alone, or null. */
const setup = ref<string | null>(null)
const info = ref<ScriptRow | null>(null)
const tabs = ref<Tab[]>([])
const tabKey = ref<string | null>(null)
const running = ref(false)
const starting = ref(false)
const runOn = ref<'running' | 'new'>('new')
const runSim = ref<string | null>(null)
const shownRun = ref<string | null>(null)
const outputEl = ref<HTMLPreElement>()

const tab = computed(() => tabs.value.find(t => t.key === tabKey.value) ?? null)
/** The scripts to run, and those other scripts include (startup, globals). */
const runnable = computed(() => catalog.scripts.filter(s => !s.included_by?.length))
const included = computed(() => catalog.scripts.filter(s => !!s.included_by?.length))
const includedBy = computed(() => catalog.scripts.find(s => s.name === current.value)?.included_by ?? [])
function tabDirty(t: Tab) { return !t.readonly && t.text !== t.saved }
const anyDirty = computed(() => tabs.value.some(tabDirty))
/** What a new simulation runs on: the Nodes tab's shown layers, the active
 *  one first, merged when there are several. */
const runLayers = computed<string[]>(() => {
  const active = nodes.nodeset?.name
  const shown = nodes.layers.filter(l => l.shown && l.name !== active).map(l => l.name)
  return [...(active ? [active] : []), ...shown]
})
const runningNames = computed(() => sim.sims.filter(s => s.state === 'running').map(s => s.name))
const runReady = computed(() => (runOn.value === 'running' ? !!runSim.value
  : !!(nodes.geodata && runLayers.value.length)))
const runOutput = computed(() => (shownRun.value ? catalog.runs[shownRun.value] ?? null : null))

watch(running, (open) => {
  if (!open) return
  runSim.value = runSim.value ?? sim.selected ?? runningNames.value[0] ?? null
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

function guard(go: () => void, kept?: () => void) {
  if (!anyDirty.value) { go(); return }
  const what = current.value ? `${current.value} and what it imports` : `${setup.value}'s setup`
  quasar.dialog({ title: 'Unsaved changes', message: `Discard the changes to ${what}?`,
                  cancel: true, persistent: true }).onOk(go).onCancel(() => kept?.())
}

/** A file of the store as a tab: a script, when it is one of scripts/, else read-only. */
function moduleTab(path: string, library: boolean, text: string): Tab {
  const script = library ? null : path.replace(/^scripts\//, '').replace(/\.py$/, '')
  return { key: `ref:${path}`, label: path.replace(/^scripts\//, ''), script, nodeset: null,
           readonly: library, fresh: false, text, saved: text }
}

function setupTab(name: string, text: string, exists: boolean): Tab {
  return { key: `setup:${name}`, label: `nodesets/${name}.py`, script: null, nodeset: name,
           readonly: false, fresh: !exists, text, saved: text }
}

/** A script in its tab, a tab for each file it imports and for
 *  scripts/globals.py, and one for each nodeset of the world it would run on
 *  that has a setup file. */
async function show(name: string, row: ScriptRow, text: string) {
  current.value = name
  setup.value = null
  if (route.query.setup !== undefined) void router.replace({ query: {} })
  info.value = row
  const opened: Tab[] = [{ key: `script:${name}`, label: `${name}.py`, script: name, nodeset: null,
                           readonly: false, fresh: false, text, saved: text }]
  const refs = row.references ?? []
  const paths = [...refs.map(r => ({ path: r.path, library: r.library })),
                 ...(refs.some(r => r.path === GLOBALS_PATH) ? [] : [{ path: GLOBALS_PATH, library: false }])]
  for (const ref of paths) {
    const r = await request('module_open', { path: ref.path })
    if (r.ok) opened.push(moduleTab(ref.path, ref.library, r.text as string))
  }
  for (const layer of runLayers.value) {
    const r = await request('nodeset_setup_open', { name: layer })
    if (r.ok && r.exists) opened.push(setupTab(layer, r.text as string, true))
  }
  tabs.value = opened
  tabKey.value = opened[0]!.key
}

/** One nodeset's setup file alone, its template when it has none yet. */
async function showSetup(name: string) {
  const r = await request('nodeset_setup_open', { name })
  if (!r.ok) { tell(r.error); return }
  current.value = null
  info.value = null
  setup.value = name
  tabs.value = [setupTab(name, r.text as string, !!r.exists)]
  tabKey.value = tabs.value[0]!.key
}

function openScript(name: string) {
  guard(async () => {
    const r = await request('script_open', { name })
    if (!r.ok) { tell(r.error); return }
    await show(name, r.script as ScriptRow, r.text as string)
  })
}

watch(() => route.query.setup, (name) => {
  if (typeof name !== 'string' || !name || name === setup.value) return
  sim.show('scripts')
  // Kept: the query goes, so asking for the same setup again comes back here.
  guard(() => { void showSetup(name) }, () => { void router.replace({ query: {} }) })
}, { immediate: true })

async function saveTab(t: Tab): Promise<boolean> {
  if (t.readonly || (!tabDirty(t) && !t.fresh)) return true
  if (t.nodeset) {
    const r = await request('nodeset_setup_save', { name: t.nodeset, text: t.text })
    if (!r.ok) { tell(r.error); return false }
    t.saved = t.text
    t.fresh = false
    return true
  }
  if (!t.script) return true
  const r = await request('script_save', { name: t.script, text: t.text })
  if (!r.ok) { tell(r.error); return false }
  t.saved = t.text
  if (t.script === current.value) info.value = r.script as ScriptRow
  return true
}

async function save() {
  if (tab.value && await saveTab(tab.value)) {
    tell(null, 'saved')
    void catalog.refresh()
  }
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
    await show(name, r.script as ScriptRow, r.text as string)
    void catalog.refresh()
  }))
}

function askSaveAs() {
  const t = tab.value
  if (!t || t.readonly) return
  askName('Save script as', async (name) => {
    const r = await request('script_save_as', { name, text: t.text })
    if (!r.ok) { tell(r.error); return }
    await show(name, r.script as ScriptRow, t.text)
    void catalog.refresh()
  })
}

async function run() {
  for (const t of tabs.value) if (!await saveTab(t)) return
  if (runOn.value === 'new') {
    const error = await saveIfEditing(nodes.nodeset?.name ?? null)
    if (error) { tell(error); return }
  }
  starting.value = true
  const r = await request('script_run', runOn.value === 'running'
    ? { name: current.value, sim: runSim.value }
    : { name: current.value, geodata: nodes.geodata, nodesets: runLayers.value })
  starting.value = false
  if (!r.ok) { tell(r.error); return }
  running.value = false
  if (r.run) shownRun.value = r.run as string
  // Over to its live map, framed on its nodes once they are there: the
  // script starts its simulation itself, a moment from now.
  if (r.simulation) {
    sim.fitWanted = true
    sim.attach(r.simulation as string, true)
  }
}

async function stopRun(id: string) {
  const r = await request('script_stop', { run: id })
  if (!r.ok) tell(r.error)
}

/** Tab in the editor puts four spaces in, as Python wants. */
function indent(event: KeyboardEvent) {
  const t = tab.value
  if (!t || t.readonly) return
  const el = event.target as HTMLTextAreaElement
  const start = el.selectionStart, end = el.selectionEnd
  t.text = `${t.text.slice(0, start)}    ${t.text.slice(end)}`
  void nextTick(() => { el.selectionStart = el.selectionEnd = start + 4 })
}
</script>

<style scoped>
.sp { display: flex; height: 100%; }
.sp-world { font-size: 12px; }
.sp-world > div { display: flex; gap: 10px; padding: 2px 0; }
.sp-world span { color: #6b7280; width: 110px; flex: none; }
.sp-world b { font-weight: 500; color: #e5e7eb; }
.sp-world .sp-world-note { color: #6b7280; font-size: 11px; }
.sp-subhead { padding-top: 12px; }
.sp-list { width: 280px; flex: none; border-right: 1px solid #262c35; overflow-y: auto; padding: 10px 0; }
.sp-head { display: flex; align-items: center; padding: 0 12px 4px; }
.sp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.sp-doc { font-size: 11px; color: #6b7280 !important; }
.sp-active { background: #1a2029; color: #fff; }
.sp-main-pane { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; }
.sp-bar { min-height: 38px; gap: 6px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.sp-tabs { background: #13171d; border-bottom: 1px solid #262c35; flex: none; font-size: 12px; }
.sp-ro { margin-left: 6px; font-size: 10px; color: #6b7280; border: 1px solid #374151; border-radius: 3px; padding: 0 4px; }
.sp-title { font-size: 14px; font-weight: 500; padding: 0 8px; }
.sp-dirty { color: #f59e0b; }
.sp-split { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }
.sp-editor {
  flex: 1 1 60%; min-height: 0; resize: none; border: none; outline: none; padding: 10px 14px;
  background: #0e1116; color: #d1d5db; font: 13px/1.5 ui-monospace, monospace; tab-size: 4;
}
.sp-readonly { color: #9ca3af; background: #0b0d10; }
.sp-output { flex: 1 1 40%; min-height: 0; display: flex; flex-direction: column; border-top: 1px solid #262c35; }
.sp-output-head { display: flex; align-items: center; padding: 2px 8px 2px 12px; font-size: 12px; color: #9ca3af; }
.sp-output pre {
  flex: 1 1 auto; margin: 0; overflow: auto; padding: 6px 12px; background: #0b0d10;
  font: 12px/1.4 ui-monospace, monospace; color: #c7ccd4; white-space: pre-wrap; word-break: break-word;
}
.sp-intro { padding: 16px 20px; max-width: 720px; font-size: 13px; color: #9ca3af; line-height: 1.6; }
</style>
