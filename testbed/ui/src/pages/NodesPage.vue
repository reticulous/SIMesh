<template>
  <q-page class="np">
    <q-toolbar class="np-bar">
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

      <template v-else-if="nodes.open">
        <div class="np-title">
          {{ nodes.nodeset?.name ?? 'unnamed' }}<span v-if="nodes.dirty" class="np-dirty"> •</span>
          <span class="np-sub">{{ nodes.names.length }} nodes on {{ nodes.geodata }}</span>
        </div>
        <q-btn flat dense no-caps label="Save" :disable="!nodes.dirty && !!nodes.nodeset?.name"
               :color="nodes.dirty ? 'amber' : undefined" @click="save" />
        <q-btn v-if="socket.front && nodes.nodeset?.name" flat dense no-caps label="Edit setup"
               @click="editSetup(nodes.nodeset.name)">
          <q-tooltip>This nodeset's own setup script, nodesets/{{ nodes.nodeset.name }}.py, which the startup script runs for it</q-tooltip>
        </q-btn>
      </template>
      <div v-else class="np-title">
        <span class="np-sub">{{ nodes.geodata ? `no layer active on ${nodes.geodata}` : 'no geodata chosen' }}</span>
      </div>
      <q-space />
      <template v-if="nodes.attached">
        <span class="np-count">{{ sim.running }}/{{ sim.nodeList.length }} up</span>
      </template>
      <PlaceSearch @go="p => map?.goTo(p.x, p.y, 1500)" />
      <DisplayMenu view="nodes" />
    </q-toolbar>

    <div class="np-body" @contextmenu.capture="pendingAt = null; pendingNode = null">
      <!-- A running simulation's clocks, big enough to read across a room. -->
      <div v-if="nodes.attached" class="np-clock" :title="timeTitle">
        <div v-if="sim.clock.mode === 'virtual'" class="np-clock-cell">
          <div class="np-clock-figure">{{ tText(sim.clock.t) }}</div>
          <div class="np-clock-label">simulated</div>
        </div>
        <div class="np-clock-cell">
          <div class="np-clock-figure" :class="{ 'np-clock-second': sim.clock.mode === 'virtual' }">{{ realNow ?? '—' }}</div>
          <div class="np-clock-label">{{ sim.clock.mode === 'virtual' ? 'real' : 'real time' }}</div>
        </div>
        <div v-if="paceLabel || planLabel" class="np-clock-foot">
          {{ [paceLabel, planLabel].filter(Boolean).join(' · ') }}
        </div>
      </div>
      <GroundMap ref="map" :nodes="mapNodes" :others="nodes.otherNodes" :offsets="nodes.offsets"
                :selected="nodes.selection"
                :pair="nodes.pair" :links="links" :coverage="coverageSource" :coverage-label="coverageLabel"
                :display="display.nodes"
                :live="nodes.attached" editable
                :view-key="nodes.attached ? 'sim' : 'ground'"
                @select="onSelect" @pick="onPick" @move="onMove"
                @link="(a: string, b: string) => (nodes.pair = [a, b])"
                @other="(layer: string) => askActivate(layer)"
                @context="onContext" />

      <div v-if="nodes.attached && !sim.loaded" class="np-empty">
        <div class="np-empty-title">Nothing loaded</div>
      </div>
      <div v-else-if="!nodes.attached && !nodes.geodata" class="np-empty">
        <div class="np-empty-title">No geodata chosen</div>
        <div class="np-empty-text">Open one on the Geodata tab: its nodesets are the layers here.</div>
      </div>
      <div v-else-if="nodes.open && !nodes.names.length" class="np-empty">
        <div class="np-empty-text">Right-click the map ▸ New node here</div>
      </div>

      <div v-if="loss" class="np-progress">
        loss table {{ loss.band }} MHz: {{ loss.done }}/{{ loss.total || '…' }} pairs
      </div>
      <div v-else-if="linkPending" class="np-progress">links of {{ nodes.selection[0] }}: computing…</div>
      <div v-else-if="linkProblem" class="np-progress np-bad">links: {{ linkProblem }}</div>

      <div class="np-left">
        <LayersPanel v-if="socket.front && !nodes.attached && nodes.geodata" @new="askNew" @import="importing = true"
                     @save-visible="askSaveVisible" @activate="askActivate" @delete="askDelete" />
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
        <div v-if="links" class="np-legend">
          <div v-if="!nodes.attached" class="np-legend-row">
            <span>links at the node's maximum power, on scripts/globals.py's radio</span>
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
          <q-item clickable v-close-popup :disable="!pendingAt || (!nodes.attached && !nodes.geodata)" @click="askPlace(false)">
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
    <WebWindow v-for="name in webs" :key="`web-${name}`" :name="name" :visible="true"
               @update:visible="v => !v && closeWeb(name)" />

    <!-- A new layer from a source: the public node maps, or a planner CSV. -->
    <q-dialog v-model="importing">
      <q-card style="min-width: 480px; max-width: 560px">
        <q-card-section class="text-subtitle2">Import a new layer</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-option-group v-model="importSource" dense :options="[
            { label: 'MeshCore map', value: 'meshcore' },
            { label: 'PotatoMesh instance', value: 'potatomesh' },
            { label: 'Planner sites (sites.csv)', value: 'sites' },
            { label: 'Deployed-network CSV', value: 'nodes' },
          ]" />
          <template v-if="importSource === 'meshcore'">
            <q-checkbox v-model="importCompanions" dense label="companions too (repeaters and room servers always)" />
            <div class="text-caption text-grey-6">
              map.meshcore.io's node list, fetched at most once a week:
              {{ meshcoreDate ? `the copy here is of ${meshcoreDate}` : 'there is no copy here yet, so this import fetches one' }}.
              An advert older than {{ MAX_AGE_DAYS }} days, or dated in the future, is left out: both are clocks never set.
            </div>
          </template>
          <template v-else-if="importSource === 'potatomesh'">
            <q-input v-model="importUrl" dense outlined label="instance address" placeholder="https://potatomesh.example" />
            <div class="text-caption text-grey-6">
              Its /api/nodes: Meshtastic and MeshCore nodes both. A position whose precision
              was cut on purpose is left out.
            </div>
          </template>
          <template v-else>
            <q-file v-model="importFile" dense outlined label="CSV file" accept=".csv,text/csv" />
            <div class="text-caption text-grey-6">
              A transmit power the CSV states is the node's maximum.
            </div>
          </template>
          <q-input v-model="importName" dense outlined label="layer name" />
          <q-input v-model.number="importHeight" type="number" dense outlined
                   label="height where the source has none (m)" hint="marked assumed" />
          <div class="text-caption text-grey-6">
            Only the nodes inside {{ nodes.geodata }}'s extent. Each is tagged with its source,
            its kind and how good its position is.
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Import" :loading="busy"
                 :disable="!importName.trim() || (csvSource && !importFile) || (importSource === 'potatomesh' && !importUrl.trim())"
                 @click="doImport" />
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
/* The Nodes tab: always the map. Standalone, it is empty until a geodata is
 * opened on the Geodata tab; then the nodesets with a node on it are layers
 * (LayersPanel): the active one is edited here, its nodes off the geodata
 * left out and kept, the other shown ones are drawn hollow, and a click on
 * one of their nodes makes its layer active. Attached to a running
 * simulation it is that run's live map, with no layers, and the same edits
 * go to the run's own copy. Links are the one selected node's, and nothing
 * else's. Coverage is a heatmap, the network's by default, or only the
 * nodes asked about. */
import { computed, nextTick, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import GroundMap from '../components/GroundMap.vue'
import NodeEditor from '../components/NodeEditor.vue'
import TagPanel from '../components/TagPanel.vue'
import LayersPanel from '../components/LayersPanel.vue'
import DisplayMenu from '../components/DisplayMenu.vue'
import PairInspector, { type PairEnd } from '../components/PairInspector.vue'
import PlaceSearch from '../components/PlaceSearch.vue'
import ConsoleWindow from '../components/ConsoleWindow.vue'
import WebWindow from '../components/WebWindow.vue'
import { useRouter } from 'vue-router'
import { NO_RADIO, useNodes, type NodeView } from '../stores/nodes'
import { whenSaved } from '../lib/unsaved'
import { useSim } from '../stores/sim'
import { useSocket } from '../stores/socket'
import { useCatalog } from '../stores/catalog'
import { useGeodata } from '../stores/geodata'
import { useDisplay } from '../stores/display'
import { useCoverage } from '../stores/coverage'
import { linksFrom, linksFromRow, type LinkRow } from '../lib/links'
import { request } from '../lib/front'
import { direction, gain, pairGain, specOf, type Antenna, type End } from '../lib/antennas'
import { useGrounds } from '../stores/grounds'
import { txDbm } from '../lib/boards'
import { cell } from '../lib/slt'
import { roofAt } from '../lib/roof'
import { etaText, phaseText, realText, tText } from '../components/runtime'
import type { GroundPoint, LinkMark, MapNode, Pick } from '../lib/marks'

const nodes = useNodes()
const sim = useSim()
const socket = useSocket()
const catalog = useCatalog()
const ground = useGeodata()
const grounds = useGrounds()
void catalog.ensureAntennas()
const display = useDisplay()
const coverage = useCoverage()
const quasar = useQuasar()
const router = useRouter()
const map = ref<InstanceType<typeof GroundMap>>()
const pendingAt = ref<GroundPoint | null>(null)
const pendingNode = ref<string | null>(null)
const importing = ref(false)
const commanding = ref(false)
const busy = ref(false)
const MAX_AGE_DAYS = 365
const importSource = ref<'meshcore' | 'potatomesh' | 'sites' | 'nodes'>('meshcore')
const csvSource = computed(() => importSource.value === 'sites' || importSource.value === 'nodes')
const importFile = ref<File | null>(null)
const importUrl = ref('')
const importCompanions = ref(false)
const importName = ref('')
const importHeight = ref(15)
const meshcoreDate = ref<string | null>(null)
watch(importing, async (open) => {
  if (!open) return
  try {
    const r = await fetch('/api/nodes/sources')
    const body = await r.json() as { meshcore?: { fetched: number | null } }
    const at = body.meshcore?.fetched
    meshcoreDate.value = at ? new Date(at * 1000).toISOString().slice(0, 10) : null
  } catch { meshcoreDate.value = null }
})
const consoles = ref<string[]>([])
const webs = ref<string[]>([])
const commandLine = ref('')
const commandSpread = ref(0)
const commandKind = ref<string | null>(null)
const waiting = ref(false)

/* Whether this map is on show: on the Nodes tab editing, or on the
 * Simulations tab as an open simulation's live map. */
const shown = computed(() => sim.view === (socket.front && nodes.attached ? 'sims' : 'nodes'))

/* The ground under the map: the run's when attached, else the one chosen. */
const groundName = computed(() => (nodes.attached ? sim.geodata?.name ?? null : nodes.geodata))
watch(() => [shown.value, groundName.value] as const, ([on, name]) => {
  if (!on) return
  // Until a geodata is chosen on the Geodata tab, the world is empty.
  if (!name) { if (!nodes.attached) void ground.show(null); return }
  // A simd on its own holds no sidecar for the page: its geodata is shown as its snapshot says.
  if (!socket.front) void ground.show(sim.geodata)
  else void ground.open(name).then(e => e && tell(e))
}, { immediate: true })

const mapNodes = computed<MapNode[]>(() => nodes.list.map(n => ({
  name: n.name, id: n.id, lat: n.lat, lon: n.lon, height_m: n.height_m, height_from: n.height_from,
  tags: n.tags, liveRole: n.liveRole, status: n.status, stale: n.stale,
})))

const loss = computed(() => (nodes.attached ? sim.progress[sim.selected ?? ''] ?? null : null))

/* Standalone, the one selected node's row and column, from the nodes as they
 * stand, saved or not: asked of the front whenever the selection or any
 * node's place changes, which keeps each pair by where both ends stand and
 * so computes only the pairs that are new. */
const hasRadio = (name: string) => !nodes.byName[name]?.tags.includes(NO_RADIO)
const linkRow = ref<LinkRow | null>(null)
const linkProblem = ref<string | null>(null)
const linkPending = ref(false)
let rowAsk = 0
watch(() => [nodes.attached, nodes.selection.length === 1 ? nodes.selection[0] : null, nodes.geodata,
             socket.front, nodes.list.map(n => `${n.name}${n.lat},${n.lon},${n.height_m}`).join(';')] as const,
      async ([attached, name, geodata, front]) => {
        const ask = ++rowAsk
        linkPending.value = false
        if (attached || !name || !geodata || !front) { linkRow.value = null; linkProblem.value = null; return }
        // Only a row that has to wait on the sidecar is worth saying so.
        const slow = setTimeout(() => { if (ask === rowAsk) linkPending.value = true }, 300)
        const records = Object.fromEntries(nodes.list.map(n => [n.name, {
          id: n.id, lat: n.lat, lon: n.lon, height_m: n.height_m, height_from: n.height_from,
          antenna: n.antenna, tags: n.tags,
        }]))
        const r = await request('links', { geodata, node: name, nodes: records })
        clearTimeout(slow)
        if (ask !== rowAsk) return
        linkPending.value = false
        linkRow.value = r.ok ? { node: r.node as string, f0_hz: r.f0_hz as number, cells: r.cells as LinkRow['cells'] } : null
        linkProblem.value = r.ok ? null : r.error ?? 'no links'
      }, { immediate: true })

/* Each node's radio: the one a running station reports, else
 * scripts/globals.py's, at the node's maximum power; a node tagged no-radio
 * has no links, to or from it. Each pair's antennas are on it as the medium
 * has them. */
function radioOf(n: NodeView) {
  const g = catalog.globals
  if (!g && !n.freq) return null
  return {
    freq: n.freq ?? (g ? g.FREQ_MHZ * 1e6 : undefined),
    bw: n.bw ?? (g ? g.BW_KHZ * 1e3 : undefined),
    sf: n.sf ?? g?.SF, power_dbm: txDbm(n.max_dbm, undefined),
  }
}

/** What every other node hears from one: attached, the ether's own list and
 *  the run's table for everyone it only interferes with; standalone, its row. */
function marksFrom(name: string, heard: Record<string, number> | null): LinkMark[] | null {
  const n = nodes.byName[name]
  if (!n || !hasRadio(name)) return null
  const radio = radioOf(n)
  if (!radio) return null
  void grounds.version
  const gainTo = (other: string) => {
    const o = nodes.byName[other]
    return o ? pairGain(catalog.antennaByType, endOf(n), endOf(o)) : 0
  }
  const table = nodes.attached ? sim.table : null
  const row = linkRow.value?.node === name ? linkRow.value : null
  const marks = table ? linksFrom(table, name, gainTo, radio, heard, sim.medium.noise_figure_db)
    : row ? linksFromRow(row, gainTo, radio, heard, sim.medium.noise_figure_db)
    : heard ? Object.entries(heard).map(([other, level]) => ({ name: other, level, decodable: true, los: null }))
    : null
  return marks && marks.filter(m => hasRadio(m.name))
}

/* The one selected node's links, every level shown. */
const links = computed<LinkMark[] | null>(() => {
  if (nodes.selection.length !== 1) return null
  const name = nodes.selection[0]!
  return marksFrom(name, nodes.attached ? sim.levels[name] ?? null : null)
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
  if (!catalog.globals) parts.push(`no radio to cover with: ${catalog.globalsError ?? 'scripts/globals.py gives none'}`)
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
  void grounds.version
  const na = nodes.byName[p[0]], nb = nodes.byName[p[1]]
  if (!na || !nb) return null
  // Each end's antenna gain toward the other, in three dimensions.
  const ea = endOf(na), eb = endOf(nb)
  const toward = (from: End, to: End) => {
    const spec = specOf(catalog.antennaByType, from.antenna)
    if (!spec) return 0
    const [az, el] = direction(from.x, from.y, from.top, to.x, to.y, to.top)
    return gain(spec, from.antenna, az, el)
  }
  const end = (n: typeof na, g: number): PairEnd =>
    ({ name: n.name, lat: n.lat, lon: n.lon, height_m: n.height_m, gain_dbi: g })
  return [end(na, toward(ea, eb)), end(nb, toward(eb, ea))]
})

/** A node's antenna tip for the patterns: where it stands, and its height
 *  over the ground there above sea level (the ground 0 until it is known). */
function endOf(n: { lat: number; lon: number; height_m: number; antenna: Antenna }): End {
  const [x, y] = ground.frame.toXY(n.lat, n.lon)
  return { x, y, top: (grounds.at(n.lat, n.lon) ?? 0) + n.height_m, antenna: n.antenna }
}
const pairCell = computed(() => {
  const p = nodes.pair
  if (!p) return null
  let loss: number | null | undefined
  const t = nodes.attached ? sim.table : null
  const row = linkRow.value
  if (t) loss = cell(t, p[0], p[1])?.loss
  else if (row?.node === p[0]) loss = row.cells[p[1]] ? row.cells[p[1]]!.to ?? Infinity : undefined
  else if (row?.node === p[1]) loss = row.cells[p[0]] ? row.cells[p[0]]!.from ?? Infinity : undefined
  if (loss === undefined || loss === null) return null
  return Number.isFinite(loss) ? `${loss.toFixed(1)} dB` : 'never heard'
})

/* ── time, when attached ── */
const planLabel = computed(() => {
  const s = sim.current
  if (!s) return null
  return [phaseText(s), etaText(s)].filter(Boolean).join(' · ') || null
})
/* Simulated T from the run's own clock, real time elapsed from the
 * registry's row (which comes every second), and how fast T runs. A
 * real-time run's simulated time is its real time: one figure is enough. */
const realNow = computed(() => (sim.current ? realText(sim.current) : null))
const paceLabel = computed(() => {
  const c = sim.clock
  if (c.mode !== 'virtual') return null
  return c.rate ? `${c.rate}× real time` : `as fast as it goes${c.observed ? `, ≈${c.observed.toFixed(1)}×` : ''}`
})

/* A script's Run comes here with `fitWanted`: frame the simulation's nodes
 * once they have arrived. */
watch(() => [sim.fitWanted, nodes.attached, mapNodes.value.length] as const, ([want, attached, count]) => {
  if (!want || !attached || !count) return
  sim.fitWanted = false
  void nextTick(() => map.value?.fitToNodes())
})
const timeTitle = computed(() =>
  sim.clock.mode === 'virtual'
    ? 'Virtual time: the ether moves T when every station is idle. Rings are drawn at the run\'s pace.'
    : 'Real time: stations and the medium run on the wall clock.')

function tell(error: string | null, done?: string) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 6000 })
  else if (done) quasar.notify({ type: 'positive', message: done, timeout: 2500 })
}

/* Making another layer active, New and Import all leave the active layer's
 * unsaved edits behind, so each asks first, and only when there is
 * something to lose. */
function guard(go: () => void) { whenSaved(quasar, go) }

/* The Layers panel, asked again only when something it depends on changes:
 * the registry brings the nodeset names every second, the same ones almost always. */
watch(() => `${socket.connected} ${socket.front} ${nodes.geodata} ${nodes.attached} ${catalog.nodesets.map(n => n.name).join()}`,
      () => { if (socket.front && !nodes.attached) void nodes.loadLayers() }, { immediate: true })

function askActivate(name: string) {
  guard(() => {
    void nodes.activate(name).then((e) => {
      tell(e)
      const row = nodes.layers.find(l => l.name === name)
      if (!e && row && row.inside < row.nodes) {
        quasar.notify({ type: 'warning', timeout: 6000,
          message: `${row.nodes - row.inside} of ${row.nodes} nodes of ${name} are outside ${nodes.geodata} and not loaded; Save keeps them` })
      }
    })
  })
}

function askDelete(name: string) {
  quasar.dialog({
    title: `Delete ${name}`,
    message: `Delete the nodeset ${name} and its own setup script? This cannot be undone.`,
    ok: { label: 'Delete', color: 'negative', flat: true, noCaps: true },
    cancel: { flat: true, noCaps: true }, persistent: true,
  }).onOk(async () => tell(await nodes.deleteNodeset(name), 'deleted'))
}

function askNew() {
  guard(() => {
    quasar.dialog({
      title: 'New layer', message: 'An empty nodeset, active. Lower-case letters, digits and hyphens.',
      prompt: { model: '', type: 'text' }, cancel: true,
    }).onOk(async (name: string) => { if (name.trim()) tell(await nodes.newLayer(name.trim())) })
  })
}

function askSaveVisible() {
  const shown = nodes.layers.filter(l => l.shown).map(l => l.name)
  quasar.dialog({
    title: 'Save visible as',
    message: shown.length > 1
      ? `One new nodeset of ${shown.join(', ')}, as they stand, top first: each node tagged with its layer, `
        + 'and of two within 5 m the upper layer\'s kept.'
      : 'The shown layer, as it stands, as a new nodeset.',
    prompt: { model: '', type: 'text' }, cancel: true,
  }).onOk(async (name: string) => { if (name.trim()) tell(await nodes.saveVisibleAs(name.trim()), 'saved') })
}

async function save() {
  if (!nodes.attached && !nodes.nodeset?.name) { askSaveAs(); return }
  tell(await nodes.save(), 'saved')
}

/** Over to the Scripts tab, on this nodeset's setup file alone. */
function editSetup(name: string) {
  guard(() => {
    void router.push({ query: { setup: name } })
    sim.show('scripts')
  })
}

function askSaveAs() {
  quasar.dialog({
    title: 'Save nodeset as', message: 'Lower-case letters, digits and hyphens.',
    prompt: { model: nodes.data?.name ?? '', type: 'text' }, cancel: true,
  }).onOk(async (name: string) => { if (name.trim()) tell(await nodes.saveAs(name.trim()), 'saved') })
}

function doImport() {
  guard(async () => {
    busy.value = true
    const fields: Record<string, unknown> = {
      name: importName.value.trim(), source: importSource.value,
      ...(importHeight.value ? { height_m: importHeight.value } : {}),
    }
    if (importSource.value === 'meshcore') Object.assign(fields, { companions: importCompanions.value, max_age_days: MAX_AGE_DAYS })
    else if (importSource.value === 'potatomesh') fields.url = importUrl.value.trim()
    else if (importFile.value) fields.text = await importFile.value.text()
    const error = await nodes.importNodes(fields)
    busy.value = false
    tell(error, error ? undefined : 'imported')
    if (!error) importing.value = false
  })
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
function openWeb(name: string) { if (!webs.value.includes(name)) webs.value.push(name) }
function closeWeb(name: string) { webs.value = webs.value.filter(n => n !== name) }

/* The selected station's levels, asked again when a moved row lands. */
watch(() => nodes.selection, (sel) => { if (nodes.attached && sel.length === 1) sim.askLevels(sel[0]!) })
watch(() => sim.nodeList.some(n => n.stale), (anyStale, was) => {
  if (was && !anyStale && nodes.selection.length === 1) sim.askLevels(nodes.selection[0]!)
})
watch(() => sim.selected, () => {
  consoles.value = []
  webs.value = []
  nodes.selection = []
})
</script>

<style scoped>
.np { display: flex; flex-direction: column; height: 100%; }
.np-bar { min-height: 38px; gap: 6px; padding-left: 4px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.np-title { font-size: 14px; font-weight: 500; padding: 0 6px; white-space: nowrap; }
.np-sim { font-size: 14px; font-weight: 500; padding: 0 8px; white-space: nowrap; display: flex; align-items: center; }
.np-sub { font-size: 12px; font-weight: 400; color: #6b7280; padding-left: 8px; }
.np-dirty { color: #f59e0b; }
.np-bad { color: #fca5a5; }
.np-clock {
  position: absolute; top: 10px; left: 50%; transform: translateX(-50%); z-index: 5;
  display: flex; flex-wrap: wrap; justify-content: center; gap: 4px 22px; padding: 8px 18px 6px;
  background: rgba(17, 20, 26, 0.86); border: 1px solid #2b313b; border-radius: 8px;
  pointer-events: none; min-width: 180px;
}
.np-clock-cell { display: flex; flex-direction: column; align-items: center; }
.np-clock-figure { font: 500 30px/1.05 ui-monospace, monospace; color: #e5e7eb; letter-spacing: 0.02em; }
.np-clock-second { color: #9ca3af; }
.np-clock-label { font-size: 10px; color: #6b7280; text-transform: uppercase; letter-spacing: 0.08em; }
.np-clock-foot { width: 100%; text-align: center; font-size: 11px; color: #7dd3fc; }
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
