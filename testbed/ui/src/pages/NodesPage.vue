<template>
  <q-page class="np">
    <!-- No nodeset open: the ones on this geodata, like the Geodata tab's list. -->
    <div v-if="listing" class="np-pick">
      <div class="np-pick-head">
        <div class="np-pick-heading">Nodesets on {{ nodes.geodata ?? '…' }}</div>
        <q-space />
        <q-btn unelevated dense no-caps color="primary" label="Import CSV…" @click="importing = true" />
      </div>
      <table class="np-pick-table">
        <thead><tr><th>Nodeset</th><th>Nodes</th><th>Tags</th></tr></thead>
        <tbody>
          <tr v-for="f in openable" :key="f.name" :class="f.error ? 'np-pick-bad' : 'np-pick-row'"
              @click="!f.error && open(f.name)">
            <td class="np-pick-name">{{ f.name }}<div v-if="f.error" class="np-bad">{{ f.error }}</div></td>
            <td>{{ f.error ? '' : f.nodes ?? 0 }}</td>
            <td class="np-pick-tags">{{ Object.keys(f.tags ?? {}).join(' ') }}</td>
          </tr>
          <tr class="np-pick-row" @click="nodes.ensureOpen()">
            <td colspan="3" class="np-pick-empty">Start empty</td>
          </tr>
        </tbody>
      </table>
    </div>

    <q-toolbar v-show="!listing" class="np-bar">
      <template v-if="nodes.attached">
        <q-btn v-if="socket.front" flat dense no-caps label="‹ Simulations" @click="sim.detach()">
          <q-tooltip>Back to the list; the simulation keeps running</q-tooltip>
        </q-btn>
        <span class="np-sim">
          {{ sim.selected || 'simulation' }}
          <span class="np-sub">{{ sim.run?.nodeset ?? 'nodes' }} on {{ sim.geodata?.name ?? '—' }}</span>
        </span>
        <q-btn flat dense no-caps label="Save nodes as nodeset…" @click="askSaveAs">
          <q-tooltip>Keep this simulation's nodes, as moved and edited in it, as a nodeset of their own</q-tooltip>
        </q-btn>
      </template>

      <template v-else>
        <q-btn v-if="socket.front" flat dense no-caps label="‹ Nodesets" @click="guard(() => nodes.close())" />
        <div class="np-title">
          {{ nodes.nodeset?.name ?? 'unnamed' }}<span v-if="nodes.dirty" class="np-dirty"> •</span>
          <span class="np-sub">{{ nodes.names.length }} nodes on {{ nodes.geodata }}</span>
        </div>
        <q-btn flat dense no-caps label="Save" :disable="!nodes.dirty && !!nodes.nodeset?.name"
               :color="nodes.dirty ? 'amber' : undefined" @click="save" />
      </template>
      <q-space />
      <template v-if="nodes.attached">
        <span v-if="planLabel" class="np-plan">{{ planLabel }}</span>
        <span class="np-time" :title="timeTitle">{{ timeLabel }}</span>
        <span class="np-count">{{ sim.running }}/{{ sim.nodeList.length }} up</span>
      </template>
      <PlaceSearch @go="p => map?.goTo(p.x, p.y, 1500)" />
      <DisplayMenu view="nodes" />
    </q-toolbar>

    <div v-show="!listing" class="np-body" @contextmenu.capture="pendingAt = null; pendingNode = null">
      <GroundMap ref="map" :nodes="mapNodes" :offsets="nodes.offsets" :selected="nodes.selection"
                :pair="nodes.pair" :links="links" :coverage="coverageSource" :coverage-label="coverageLabel"
                :display="display.nodes"
                :live="nodes.attached" editable
                :view-key="nodes.attached ? 'sim' : 'nodes'"
                @select="onSelect" @pick="onPick" @move="onMove"
                @link="(a: string, b: string) => (nodes.pair = [a, b])"
                @context="onContext" />

      <div v-if="nodes.attached && !sim.loaded" class="np-empty">
        <div class="np-empty-title">Nothing loaded</div>
      </div>
      <div v-else-if="nodes.open && !nodes.names.length" class="np-empty">
        <div class="np-empty-text">Right-click the map ▸ New node here</div>
      </div>

      <div v-if="loss" class="np-progress">
        loss table {{ loss.band }} MHz: {{ loss.done }}/{{ loss.total || '…' }} pairs
      </div>
      <div v-else-if="nodes.losses?.error && display.nodes.links" class="np-progress np-bad">
        links: {{ nodes.losses.error }}
      </div>

      <div class="np-left">
        <TagPanel />
        <div class="np-help">
          Click a node; Shift-click toggles it. Ctrl/Cmd-drag a rectangle to
          select (with Shift to add, with Alt to take out). Drag a node to move
          it, and the selection with it. Right-click for more.
        </div>
      </div>

      <!-- Out from under the display menu while it is open. -->
      <div class="np-side" :class="{ 'np-side-aside': display.menuOpen }">
        <NodeEditor @inspect="o => (nodes.pair = [nodes.selection[0]!, o])"
                    @remove="askRemove" @console="openConsole"
                    @web="openWeb" @factory="askFactory" />
        <div v-if="links && display.nodes.links" class="np-legend">
          <div v-if="!nodes.attached" class="np-legend-row">
            <span>links at the node's own power and SF</span>
          </div>
          <div class="np-legend-row">
            <span class="np-swatch" style="background: #22c55e" /> decodable
            <span class="np-swatch" style="background: #ef4444" /> interference only
            <span class="np-dash" /> Fresnel zone not clear
          </div>
        </div>
      </div>

      <q-menu context-menu touch-position>
        <q-list v-if="pendingNode" dense style="min-width: 210px">
          <q-item-label header>{{ nodes.selection.length > 1 ? `${nodes.selection.length} nodes` : pendingNode }}</q-item-label>
          <template v-if="nodes.attached && nodes.selection.length === 1">
            <q-item clickable v-close-popup @click="openConsole(pendingNode)"><q-item-section>Console</q-item-section></q-item>
            <q-item clickable v-close-popup :disable="!nodes.byName[pendingNode]?.web" @click="openWeb(pendingNode)">
              <q-item-section>Web UI</q-item-section>
            </q-item>
            <q-item clickable v-close-popup @click="sim.resetNode(pendingNode)"><q-item-section>Reset</q-item-section></q-item>
          </template>
          <template v-if="nodes.attached">
            <q-item clickable v-close-popup @click="askFactory(nodes.selection)"><q-item-section>Factory reset</q-item-section></q-item>
            <q-item clickable v-close-popup @click="intent('announce', {}, nodes.selection.length > 1 ? 30 : 0)">
              <q-item-section>Announce</q-item-section>
            </q-item>
            <q-item clickable v-close-popup @click="openCommand"><q-item-section>Run command…</q-item-section></q-item>
          </template>
          <q-separator v-if="nodes.attached" />
          <q-item clickable v-close-popup @click="askRemove(nodes.selection)">
            <q-item-section class="text-negative">
              {{ nodes.selection.length > 1 ? `Remove these ${nodes.selection.length} nodes…` : 'Remove this node…' }}
            </q-item-section>
          </q-item>
        </q-list>
        <q-list v-else dense style="min-width: 200px">
          <q-item clickable v-close-popup :disable="!pendingAt" @click="askPlace(false)">
            <q-item-section>New node here</q-item-section>
          </q-item>
          <q-item clickable v-close-popup :disable="!pendingAt || !ground.isPack" @click="askPlace(true)">
            <q-item-section>New node on this roof</q-item-section>
          </q-item>
          <q-item clickable v-close-popup :disable="!nodes.selection.length" @click="nodes.select(null)">
            <q-item-section>Clear the selection</q-item-section>
          </q-item>
          <q-item clickable v-close-popup @click="nodes.selectMany(nodes.names)">
            <q-item-section>Select all</q-item-section>
          </q-item>
          <q-item clickable v-close-popup @click="map?.fitToNodes()">
            <q-item-section>Fit to nodes</q-item-section>
          </q-item>
        </q-list>
      </q-menu>
    </div>

    <PairInspector v-if="pairEnds" :a="pairEnds[0]" :b="pairEnds[1]" :table-cell="pairCell"
                   @close="nodes.pair = null" @reverse="nodes.pair = [nodes.pair![1], nodes.pair![0]]" />
    <ConsoleWindow v-for="name in consoles" :key="name" :name="name" :visible="true"
                   @update:visible="v => !v && closeConsole(name)" />

    <!-- A nodeset from a planner CSV. -->
    <q-dialog v-model="importing">
      <q-card style="min-width: 440px">
        <q-card-section class="text-subtitle2">Import CSV as a new nodeset</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-btn-toggle v-model="importFormat" dense no-caps unelevated toggle-color="primary"
                        :options="[{ label: 'sites.csv (planner optimize)', value: 'sites' },
                                   { label: 'deployed network (planner nodes import)', value: 'nodes' }]" />
          <q-file v-model="importFile" dense outlined label="CSV file" accept=".csv,text/csv" />
          <q-input v-model="importName" dense outlined label="nodeset name" />
          <q-input v-model.number="importHeight" type="number" dense outlined label="height where the CSV has none (m)"
                   hint="marked assumed" />
          <q-select v-model="importDevice" :options="catalog.deviceChoices()" emit-value map-options dense outlined
                    label="device" />
          <div class="text-caption text-grey-6">
            Every node gets the default radio, at the transmit power the CSV states where it states one.
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Import" :loading="busy"
                 :disable="!importFile || !importName.trim()" @click="doImport" />
        </q-card-actions>
      </q-card>
    </q-dialog>

    <!-- Run command: one line on the stations chosen, all of one kind. -->
    <q-dialog v-model="commanding">
      <q-card style="min-width: 560px">
        <q-card-section class="text-subtitle2">Run command on {{ targetText }}</q-card-section>
        <q-card-section>
          <div class="row q-col-gutter-sm">
            <q-select v-if="sim.kinds.length > 1" class="col-auto" style="width: 150px"
                      v-model="commandKind" :options="sim.kinds" outlined dense
                      :disable="waiting" label="kind" />
            <q-input class="col" v-model="commandLine" outlined dense autofocus
                     label="line" :disable="waiting"
                     input-style="font-family: ui-monospace, monospace"
                     @keyup.enter="runCommand" />
            <q-input class="col-auto" style="width: 120px" v-model.number="commandSpread"
                     type="number" min="0" outlined dense :disable="waiting"
                     label="spread (s)" />
          </div>
          <div class="text-caption text-grey-6 q-mt-sm">
            <code>{name}</code>, <code>{id}</code>, <code>{addr}</code> and
            <code>{addr:&lt;node&gt;}</code> are filled in per station. A line is in its
            kind's own language, so it goes to stations of one kind.
          </div>
          <div class="text-caption text-grey-6 q-mt-xs">
            <b>spread</b> is how many seconds to scatter the stations over. Leave it
            at 0 to ask them all at once; give it 30 or 60 for anything that puts
            something on the air.
          </div>
        </q-card-section>
        <q-card-section v-if="sim.command" class="np-results">
          <div v-for="(text, node) in sim.command.results" :key="node" class="np-result">
            <span class="np-result-node">{{ node }}</span>
            <pre>{{ text || '—' }}</pre>
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Close" v-close-popup />
          <q-btn flat no-caps color="primary" label="Run" :loading="waiting"
                 :disable="!commandLine.trim()" @click="runCommand" />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </q-page>
</template>

<script setup lang="ts">
/* The Nodes tab: a nodeset on its geodata, edited on the map. Standalone it
 * lists the nodesets on the geodata chosen last on the Geodata tab until one
 * is opened or started empty, then edits that one; attached to a running simulation it is that
 * run's live map, and the same edits go to the run's own copy. Coverage is a
 * layer, the network's by default, or only the nodes asked about. */
import { computed, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import GroundMap from '../components/GroundMap.vue'
import NodeEditor from '../components/NodeEditor.vue'
import TagPanel from '../components/TagPanel.vue'
import DisplayMenu from '../components/DisplayMenu.vue'
import PairInspector, { type PairEnd } from '../components/PairInspector.vue'
import PlaceSearch from '../components/PlaceSearch.vue'
import ConsoleWindow from '../components/ConsoleWindow.vue'
import { defaultDevice, useNodes } from '../stores/nodes'
import { useSim } from '../stores/sim'
import { useSocket } from '../stores/socket'
import { useCatalog, type NodesetRow } from '../stores/catalog'
import { useGeodata } from '../stores/geodata'
import { useDisplay } from '../stores/display'
import { useCoverage } from '../stores/coverage'
import { request } from '../lib/front'
import { linksFrom } from '../lib/links'
import { cell } from '../lib/slt'
import { roofAt } from '../lib/roof'
import { etaText, phaseText, realText, tText } from '../components/runtime'
import type { GroundPoint, LinkMark, MapNode, Pick } from '../lib/marks'

const nodes = useNodes()
const sim = useSim()
const socket = useSocket()
const catalog = useCatalog()
const ground = useGeodata()
const display = useDisplay()
const coverage = useCoverage()
const quasar = useQuasar()
const map = ref<InstanceType<typeof GroundMap>>()
const pendingAt = ref<GroundPoint | null>(null)
const pendingNode = ref<string | null>(null)
const openable = ref<NodesetRow[]>([])
const importing = ref(false)
const commanding = ref(false)
const busy = ref(false)
const importFormat = ref<'sites' | 'nodes'>('nodes')
const importFile = ref<File | null>(null)
const importName = ref('')
const importHeight = ref(15)
const importDevice = ref(defaultDevice())
watch(importing, (open) => { if (open) importDevice.value = defaultDevice() })
const consoles = ref<string[]>([])
const commandLine = ref('')
const commandSpread = ref(0)
const commandKind = ref<string | null>(null)
const waiting = ref(false)

const geodataNames = computed(() => catalog.geodata.filter(g => !g.error).map(g => g.name))
/* The front's nodesets to choose from until one is open, or started empty. */
const listing = computed(() => !!socket.front && !nodes.attached && !nodes.open)

/* Whether this map is on show: on the Nodes tab editing, or on the
 * Simulations tab as an open simulation's live map. */
const shown = computed(() => sim.view === (socket.front && nodes.attached ? 'sims' : 'nodes'))

/* The ground under the map: the run's when attached, else the one chosen. */
const groundName = computed(() => (nodes.attached ? sim.geodata?.name ?? null : nodes.geodata))
watch(() => [shown.value, groundName.value] as const, ([on, name]) => {
  if (!on || !name) return
  // A simd on its own holds no sidecar for the page: its geodata is shown as its snapshot says.
  if (!socket.front) void ground.show(sim.geodata)
  else void ground.open(name).then(e => e && tell(e))
}, { immediate: true })
watch(() => catalog.geodata.length, () => {
  if (!nodes.geodata && geodataNames.value.length) nodes.geodata = geodataNames.value[0]!
}, { immediate: true })

const mapNodes = computed<MapNode[]>(() => nodes.list.map(n => ({
  name: n.name, id: n.id, lat: n.lat, lon: n.lon, height_m: n.height_m, height_from: n.height_from,
  tags: n.tags, role: n.role, liveRole: n.liveRole, status: n.status, stale: n.stale,
})))

const loss = computed(() => (nodes.attached ? sim.progress[sim.selected ?? ''] ?? null
  : nodes.losses?.running ? nodes.losses : null))

/* The links layer needs the saved nodeset's loss table: computed (or taken
 * from the cache) when a single node is selected with the layer on. An edit
 * that moves a node drops the table, and the next save brings it back. */
watch(() => [display.nodes.links, nodes.selection.length === 1, nodes.nodeset?.name,
             nodes.dirty, !!nodes.table] as const,
      ([links, one, name, dirty, have]) => {
        if (nodes.attached || !links || !one || !name || dirty || have) return
        if (nodes.losses?.running || nodes.losses?.error) return
        void nodes.computeLosses('868')
      })

/* Links from the one selected node: the ether's own list when a simulation
 * runs, and everyone else it only interferes with from the table. */
const links = computed<LinkMark[] | null>(() => {
  if (nodes.selection.length !== 1) return null
  const name = nodes.selection[0]!
  const n = nodes.byName[name]
  if (!n) return null
  const heard = nodes.attached ? sim.levels[name] ?? null : null
  const table = nodes.linkTable
  if (table) {
    const gains = Object.fromEntries(nodes.list.map(x => [x.name, x.antenna?.gain_dbi ?? 0]))
    const r = n.radio ?? {}
    return linksFrom(table, name, gains, {
      freq: n.freq ?? (r.freq_mhz ? r.freq_mhz * 1e6 : undefined),
      bw: n.bw ?? (r.bw_khz ? r.bw_khz * 1e3 : undefined),
      sf: n.sf ?? r.sf, power_dbm: r.tx_dbm,
    }, heard, sim.medium.noise_figure_db)
  }
  return heard ? Object.entries(heard).map(([other, level]) => ({ name: other, level, decodable: true, los: null })) : null
})

/* Coverage: asked for only while this tab is on show and the heatmap is
 * coverage. It is the selected nodes' when any are selected, else the whole
 * network's, and the map's key says which. */
const coverageNodes = computed(() => {
  const sel = nodes.selection
  return sel.length ? nodes.list.filter(n => sel.includes(n.name)) : nodes.list
})
const coverageLabel = computed(() => {
  const sel = nodes.selection
  const parts = [sel.length === 0 ? 'coverage of the whole network'
    : sel.length === 1 ? `coverage of ${sel[0]}` : `coverage of the ${sel.length} selected`]
  if (coverage.pending.length) parts.push(`computing ${coverage.pending.length}…`)
  if (coverage.problem) parts.push(coverage.problem)
  return parts.join(' · ')
})
const coverageWanted = computed(() => shown.value && display.nodes.coverage && !!ground.current)
watch(() => [coverageWanted.value, ground.current?.name,
             coverageNodes.value.map(n => `${n.name}${n.lat},${n.lon},${n.height_m}`).join(';')] as const,
      ([wanted]) => { if (wanted) void coverage.ensure(ground.current, coverageNodes.value) },
      { immediate: true })
const coverageSource = computed(() => {
  const g = ground.current
  if (!coverageWanted.value || !g) return null
  void coverage.version
  return coverage.source(g, ground.frame, coverageNodes.value, sim.medium.noise_figure_db)
})

const pairEnds = computed<[PairEnd, PairEnd] | null>(() => {
  const p = nodes.pair
  if (!p) return null
  const end = (name: string): PairEnd | null => {
    const n = nodes.byName[name]
    return n ? { name, lat: n.lat, lon: n.lon, height_m: n.height_m, gain_dbi: n.antenna?.gain_dbi ?? 0 } : null
  }
  const a = end(p[0]), b = end(p[1])
  return a && b ? [a, b] : null
})
const pairCell = computed(() => {
  const t = nodes.linkTable, p = nodes.pair
  if (!t || !p) return null
  const c = cell(t, p[0], p[1])
  return c ? (Number.isFinite(c.loss) ? `${c.loss.toFixed(1)} dB` : 'never heard') : null
})

/* ── time, when attached ── */
const planLabel = computed(() => {
  const s = sim.current
  if (!s) return null
  return [phaseText(s), etaText(s)].filter(Boolean).join(' · ') || null
})
/* Simulated T from the run's own clock, real time elapsed from the
 * registry's row (which comes every second), and how fast T runs. */
const timeLabel = computed(() => {
  const c = sim.clock
  const row = sim.current
  const real = row ? realText(row) : null
  // A real-time run's simulated time is its real time: one figure is enough.
  if (c.mode !== 'virtual') return real ? `${real} real time` : 'real time'
  const parts = [`T ${tText(c.t)} simulated`, real ? `${real} real` : null]
  if (c.rate) parts.push(`${c.rate}×`)
  else parts.push(`max${c.observed ? ` ≈${c.observed.toFixed(1)}×` : ''}`)
  return parts.filter(Boolean).join(' · ')
})
const timeTitle = computed(() =>
  sim.clock.mode === 'virtual'
    ? 'Virtual time: the ether moves T when every station is idle. Rings are drawn at the run\'s pace.'
    : 'Real time: stations and the medium run on the wall clock.')

function tell(error: string | null, done?: string) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 6000 })
  else if (done) quasar.notify({ type: 'positive', message: done, timeout: 2500 })
}

/* New, Open, Import and Close all drop unsaved edits, so each asks first,
 * and only when there is something to lose. */
function guard(go: () => void) {
  if (!nodes.dirty) { go(); return }
  quasar.dialog({
    title: 'Unsaved changes',
    message: `${nodes.nodeset?.name ?? 'This nodeset'} has changes that have not been saved. Discard them?`,
    cancel: true, persistent: true,
  }).onOk(go)
}

/** The nodesets with a node on the tab's geodata. */
async function loadOpenable() {
  if (!socket.front || !nodes.geodata) return
  const r = await request('nodeset_list', { geodata: nodes.geodata })
  if (!r.ok) { tell(r.error ?? 'no listing'); return }
  const rows = r.nodesets as (NodesetRow & { inside?: boolean })[]
  openable.value = rows.filter(n => n.inside || n.error)
}

/* Asked again only when something it depends on changes: the registry
 * brings the nodeset names every second, the same ones almost always. */
watch(() => `${socket.connected} ${nodes.geodata} ${listing.value} ${catalog.nodesets.map(n => n.name).join()}`,
      () => { if (listing.value) void loadOpenable() }, { immediate: true })

async function open(name: string) { tell(await nodes.openNodeset(name)) }

async function save() {
  if (!nodes.attached && !nodes.nodeset?.name) { askSaveAs(); return }
  tell(await nodes.save(), 'saved')
}

function askSaveAs() {
  quasar.dialog({
    title: 'Save nodeset as', message: 'Lower-case letters, digits and hyphens.',
    prompt: { model: nodes.data?.name ?? '', type: 'text' }, cancel: true,
  }).onOk(async (name: string) => { if (name.trim()) tell(await nodes.saveAs(name.trim()), 'saved') })
}

async function doImport() {
  if (!importFile.value) return
  busy.value = true
  const error = await nodes.importCsv(importFormat.value, await importFile.value.text(), importName.value.trim(),
                                      importHeight.value || null, importDevice.value)
  busy.value = false
  tell(error, error ? undefined : 'imported')
  if (!error) importing.value = false
}

function onSelect(name: string | null, how: Pick) { nodes.select(name, how) }
function onPick(names: string[], how: Pick) { nodes.selectMany(names, how) }
function onMove(name: string, lat: number, lon: number, settle: boolean) { nodes.move(name, lat, lon, settle) }

function onContext(at: GroundPoint, name: string | null) {
  pendingAt.value = at
  pendingNode.value = name
  if (name && !nodes.selection.includes(name)) nodes.select(name)
}

/** Ask a name, and place a node at the point: on the ground, or on the roof there. */
function askPlace(onRoof: boolean) {
  const at = pendingAt.value
  if (!at) return
  let n = nodes.names.length + 1
  while (nodes.byName[`n${String(n).padStart(3, '0')}`]) n++
  quasar.dialog({
    title: 'New node',
    message: 'A name: lower-case letters, digits and hyphens. It is the station\'s hostname and its reference everywhere.',
    prompt: { model: `n${String(n).padStart(3, '0')}`, type: 'text' }, cancel: true,
  }).onOk(async (name: string) => {
    name = name.trim()
    if (!/^[a-z0-9][a-z0-9-]*$/.test(name)) { tell(`"${name}" is not a node name`); return }
    if (!nodes.place(name, at.lat, at.lon)) { tell(`there is already a node called ${name}`); return }
    if (onRoof) await putOnRoof(name, at)
  })
}

async function putOnRoof(name: string, at: GroundPoint) {
  const base = ground.sidecar
  if (!base) return
  try {
    const roof = await roofAt(base, at)
    if (!roof) { tell('no roof and no clutter height there'); return }
    nodes.setMany([name], { lat: at.lat, lon: at.lon, ...roof })
  } catch (e) {
    tell((e as Error).message)
  }
}

function askRemove(names: string[]) {
  if (!names.length) return
  quasar.dialog({
    title: names.length === 1 ? `Remove ${names[0]}` : `Remove ${names.length} nodes`,
    message: nodes.attached
      ? 'Stop them, take them out of this run\'s nodeset and delete their state? This cannot be undone.'
      : 'Take them out of the nodeset, with their offsets?',
    cancel: true, persistent: true,
  }).onOk(() => nodes.remove(names))
}

function askFactory(names: string[] | null) {
  quasar.dialog({
    title: 'Factory reset',
    message: `Wipe ${names ? names.join(', ') : 'every station'}'s state and set it up again: `
           + 'identities, keys, paths and message history go.',
    cancel: true, persistent: true,
  }).onOk(() => {
    if (names) for (const n of names) sim.factoryResetNode(n)
    else sim.factoryResetAll()
  })
}

/* Commands go to the selection, or to every station when nothing is selected. */
const targetText = computed(() => (nodes.selection.length
  ? (nodes.selection.length === 1 ? nodes.selection[0]! : `${nodes.selection.length} selected`) : 'all stations'))
function targets() { return nodes.selection.length ? [...nodes.selection] : null }

function openCommand() { commanding.value = true }
function runCommand() {
  const line = commandLine.value.trim()
  if (!line || waiting.value) return
  waiting.value = true
  sim.runCommand(line, Number(commandSpread.value) || 0,
                 sim.kinds.length > 1 ? commandKind.value : null, targets())
}
function intent(verb: string, args: Record<string, unknown>, spread: number) {
  sim.runIntent(verb, args, spread, targets())
  commandLine.value = ''
  commanding.value = true
}
// The replies arrive together, as one message, so the wait ends when they do.
watch(() => sim.command, () => { waiting.value = false })
watch(() => sim.kinds, (list) => {
  if (!commandKind.value || !list.includes(commandKind.value)) commandKind.value = list[0] ?? null
}, { immediate: true })

function openConsole(name: string) { if (!consoles.value.includes(name)) consoles.value.push(name) }
function closeConsole(name: string) { consoles.value = consoles.value.filter(n => n !== name) }
function openWeb(name: string) { window.open(sim.stationUrl(name), '_blank') }

/* The selected station's levels, asked again when a moved row lands. */
watch(() => nodes.selection, (sel) => { if (nodes.attached && sel.length === 1) sim.askLevels(sel[0]!) })
watch(() => sim.nodeList.some(n => n.stale), (anyStale, was) => {
  if (was && !anyStale && nodes.selection.length === 1) sim.askLevels(nodes.selection[0]!)
})
watch(() => sim.selected, () => {
  consoles.value = []
  nodes.selection = []
})
</script>

<style scoped>
.np { display: flex; flex-direction: column; height: 100%; }
.np-bar { min-height: 38px; gap: 6px; padding-left: 4px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.np-pick { padding: 16px 20px 32px; max-width: 1100px; overflow-y: auto; }
.np-pick-head { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; }
.np-pick-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.np-pick-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.np-pick-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35;
}
.np-pick-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.np-pick-row { cursor: pointer; }
.np-pick-row:hover td { background: #1b2028; }
.np-pick-row .np-pick-name { color: #7dd3fc; font-weight: 500; }
.np-pick-bad td { color: #6b7280; }
.np-pick-tags { font-size: 11px; color: #9ca3af; }
.np-pick-empty { color: #9ca3af; font-style: italic; }
.np-title { font-size: 14px; font-weight: 500; padding: 0 6px; white-space: nowrap; }
.np-sim { font-size: 14px; font-weight: 500; padding: 0 8px; white-space: nowrap; display: flex; align-items: center; }
.np-sub { font-size: 12px; font-weight: 400; color: #6b7280; padding-left: 8px; }
.np-dirty { color: #f59e0b; }
.np-bad { color: #fca5a5; }
.np-plan { font: 11px ui-monospace, monospace; color: #7dd3fc; }
.np-time { font: 11px ui-monospace, monospace; color: #a78bfa; }
.np-count { font: 11px ui-monospace, monospace; color: #6b7280; }
.np-body { position: relative; flex: 1 1 auto; min-height: 0; }
.np-side { position: absolute; top: 12px; right: 12px; display: flex; flex-direction: column; gap: 8px;
  transition: right 0.15s ease; }
.np-side-aside { right: 304px; }
.np-left { position: absolute; top: 12px; left: 12px; display: flex; flex-direction: column; gap: 8px; }
.np-help { width: 200px; font-size: 10px; line-height: 1.4; color: #6b7280;
  background: rgba(18, 20, 23, 0.8); padding: 4px 6px; border-radius: 3px; }
.np-progress {
  position: absolute; top: 12px; left: 50%; transform: translateX(-50%); font: 11px ui-monospace, monospace;
  color: #fbbf24; background: rgba(18, 20, 23, 0.85); padding: 3px 8px; border-radius: 3px;
}
.np-legend {
  background: rgba(27, 31, 38, 0.92); border: 1px solid #2b313b; border-radius: 4px;
  padding: 4px 10px; font-size: 11px; color: #9ca3af; width: 360px;
}
.np-legend-row { display: flex; align-items: center; gap: 6px; }
.np-swatch { display: inline-block; width: 14px; height: 3px; margin-left: 6px; }
.np-dash { display: inline-block; width: 16px; border-top: 2px dashed #9ca3af; margin-left: 6px; }
.np-empty {
  position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center;
  justify-content: center; gap: 6px; pointer-events: none; text-align: center;
}
.np-empty-title { font-size: 15px; color: #9ca3af; padding: 6px 0; }
.np-empty-text { font-size: 12px; color: #6b7280; max-width: 420px; line-height: 1.5; }
.np-results { max-height: 46vh; overflow-y: auto; border-top: 1px solid #2b313b; }
.np-result { display: flex; gap: 10px; padding: 3px 0; }
.np-result-node { flex: none; width: 84px; font: 12px ui-monospace, monospace; color: #7dd3fc; }
.np-result pre {
  margin: 0; font: 12px ui-monospace, monospace; color: #c7ccd4;
  white-space: pre-wrap; word-break: break-word;
}
</style>
