<template>
  <div class="lp">
    <div class="lp-head">
      <span class="lp-title">Layers</span>
      <q-space />
      <q-btn flat dense no-caps size="sm" label="New" @click="emit('new')" />
      <q-btn flat dense no-caps size="sm" label="Import…" @click="emit('import')" />
      <q-btn flat dense no-caps size="sm" label="Save visible as…" :disable="!anyShown" @click="emit('saveVisible')" />
    </div>
    <div v-if="nodes.active === ''" class="lp-row lp-active">
      <span class="lp-dot">●</span><span class="lp-eye">👁</span>
      <span class="lp-name">unnamed</span>
      <span class="lp-count">{{ nodes.names.length }}</span>
      <span class="lp-dirty">•</span>
    </div>
    <div v-for="(l, i) in nodes.layers" :key="l.name" class="lp-row"
         :class="{ 'lp-active': l.name === nodes.active }">
      <span class="lp-dot">{{ l.name === nodes.active ? '●' : '○' }}</span>
      <span class="lp-eye" :class="{ 'lp-click': l.name !== nodes.active }"
            :title="l.name === nodes.active ? 'the active layer is always shown' : l.shown ? 'hide' : 'show'"
            @click="nodes.toggleLayer(l.name)">{{ l.shown ? '👁' : '·' }}</span>
      <span class="lp-swatch" :style="l.name === nodes.active ? {} : { borderColor: nodes.colourOf(l.name) }"
            :title="l.name === nodes.active ? 'the active layer: its nodes are drawn filled'
              : 'the colour this layer\'s nodes are drawn in, hollow'" />
      <span class="lp-name lp-click"
            :title="l.name === nodes.active ? 'the active layer' : 'make it the active layer'"
            @click="l.name !== nodes.active && emit('activate', l.name)">{{ l.name }}</span>
      <span class="lp-count" :class="{ 'lp-partly': l.inside < l.nodes }"
            :title="l.inside < l.nodes ? `${l.nodes - l.inside} of ${l.nodes} nodes are outside the geodata and will not be loaded` : ''">
        {{ l.name === nodes.active ? nodes.names.length : l.inside }}<template v-if="l.inside < l.nodes">/{{ l.nodes }}</template>
      </span>
      <span class="lp-dirty">{{ l.name === nodes.active && nodes.dirty ? '•' : '' }}</span>
      <span class="lp-up lp-click" :style="{ visibility: i ? 'visible' : 'hidden' }"
            title="up: earlier in Save visible as" @click="nodes.raiseLayer(l.name)">▲</span>
      <span class="lp-del lp-click" title="delete this nodeset" @click="emit('delete', l.name)">🗑</span>
    </div>
    <div v-if="!nodes.layers.length && nodes.active !== ''" class="lp-none">
      No nodeset has a node here yet: New, or Import….
    </div>
  </div>
</template>

<script setup lang="ts">
/* The Layers panel: every nodeset with a node on the geodata, one row each.
 * The eye shows or hides a layer; the ring is the colour its nodes are drawn
 * in; the name makes it the active one, whose nodes are edited (the page
 * asks first about unsaved edits); the count is its nodes on the geodata,
 * of how many when some are off it; ▲ moves it up, which is earlier in Save
 * visible as, where the earlier layer's node wins; the bin deletes it. */
import { computed } from 'vue'
import { useNodes } from '../stores/nodes'

const emit = defineEmits<{ new: []; import: []; saveVisible: []; activate: [name: string]; delete: [name: string] }>()
const nodes = useNodes()
const anyShown = computed(() => nodes.active === '' || nodes.layers.some(l => l.shown))
</script>

<style scoped>
.lp {
  width: 280px; background: rgba(27, 31, 38, 0.92); border: 1px solid #2b313b; border-radius: 4px;
  padding: 4px 6px 6px; font-size: 12px; color: #d1d5db; max-height: 40vh; overflow-y: auto;
}
.lp-head { display: flex; align-items: center; gap: 2px; margin-bottom: 2px; }
.lp-title { font-size: 11px; color: #9ca3af; font-weight: 500; padding-left: 2px; }
.lp-row { display: flex; align-items: center; gap: 6px; padding: 2px 2px; border-radius: 3px; }
.lp-row:hover { background: #20252d; }
.lp-active { background: #1f2630; }
.lp-dot { width: 10px; color: #a78bfa; font-size: 10px; }
.lp-eye { width: 16px; text-align: center; }
.lp-name { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.lp-active .lp-name { font-weight: 500; color: #f3f4f6; }
.lp-count { font: 11px ui-monospace, monospace; color: #6b7280; }
.lp-dirty { width: 8px; color: #f59e0b; }
.lp-up { font-size: 9px; color: #6b7280; }
.lp-swatch { width: 9px; height: 9px; border-radius: 50%; border: 2px solid transparent; flex: none; }
.lp-active .lp-swatch { background: #e5e7eb; }
.lp-partly { color: #f59e0b; }
.lp-del { font-size: 10px; opacity: 0.45; }
.lp-del:hover { opacity: 1; }
.lp-click { cursor: pointer; }
.lp-none { font-size: 11px; color: #6b7280; padding: 2px; }
</style>
