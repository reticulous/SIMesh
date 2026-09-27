<template>
  <q-card v-if="nodes.tags.length" class="tags" flat bordered>
    <div class="tags-head">
      <span>Tags</span>
      <q-space />
      <span class="tags-dim">{{ nodes.selection.length }}/{{ nodes.names.length }} selected</span>
    </div>
    <div v-for="[tag, count] in nodes.tags" :key="tag" class="tags-row">
      <span class="tags-name" :class="{ 'tags-all': allIn(tag) }">{{ tag }}</span>
      <span class="tags-dim">{{ count }}</span>
      <q-space />
      <q-btn flat dense round size="xs" @click="nodes.selectTag(tag, 'add')">
        <b>+</b><q-tooltip>Add every node tagged {{ tag }} to the selection</q-tooltip>
      </q-btn>
      <q-btn flat dense round size="xs" @click="nodes.selectTag(tag, 'remove')">
        <b>−</b><q-tooltip>Take every node tagged {{ tag }} out of the selection</q-tooltip>
      </q-btn>
    </div>
  </q-card>
</template>

<script setup lang="ts">
/* Every tag in the nodeset with how many nodes carry it, and a way to add
 * the nodes carrying one to the selection or take them out of it. */
import { useNodes } from '../stores/nodes'

const nodes = useNodes()

function allIn(tag: string) {
  const tagged = nodes.list.filter(n => n.tags.includes(tag)).map(n => n.name)
  return tagged.length > 0 && tagged.every(n => nodes.selection.includes(n))
}
</script>

<style scoped>
.tags { width: 200px; background: rgba(27, 31, 38, 0.94); border-color: #2b313b; max-height: 40vh; overflow-y: auto; }
.tags-head { display: flex; align-items: center; padding: 4px 8px; font-size: 11px; color: #9ca3af;
  text-transform: uppercase; letter-spacing: 0.06em; }
.tags-row { display: flex; align-items: center; gap: 6px; padding: 0 4px 0 10px; font-size: 12px; }
.tags-name { color: #c4b5fd; }
.tags-all { color: #fff; font-weight: 600; }
.tags-dim { color: #6b7280; font-size: 11px; }
</style>
