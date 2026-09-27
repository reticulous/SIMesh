<template>
  <q-dialog :model-value="run !== null" @update:model-value="v => !v && emit('close')">
    <q-card class="rd">
      <q-card-section class="rd-head">
        <span class="text-subtitle2">Report</span>
        <span class="rd-run">{{ run }}</span>
        <q-space />
        <q-btn flat dense round size="sm" v-close-popup>×</q-btn>
      </q-card-section>
      <q-card-section class="rd-body">
        <div v-if="error" class="rd-bad">{{ error }}</div>
        <!-- eslint-disable-next-line vue/no-v-html -- the run's own report, rendered from its Markdown -->
        <div v-else class="rd-md" v-html="html" />
      </q-card-section>
    </q-card>
  </q-dialog>
</template>

<script setup lang="ts">
/* A run's report.md, as its script's report(run_dir) wrote it. */
import { ref, watch } from 'vue'
import { marked } from 'marked'

const props = defineProps<{ run: string | null }>()
const emit = defineEmits<{ close: [] }>()
const html = ref('')
const error = ref<string | null>(null)

watch(() => props.run, async (run) => {
  html.value = ''
  error.value = null
  if (!run) return
  try {
    const r = await fetch(`/api/report?run=${encodeURIComponent(run)}`)
    if (!r.ok) { error.value = await r.text(); return }
    html.value = await marked.parse(await r.text())
  } catch (e) {
    error.value = (e as Error).message
  }
}, { immediate: true })
</script>

<style scoped>
.rd { width: 820px; max-width: 94vw; }
.rd-head { display: flex; align-items: center; gap: 10px; padding-bottom: 0; }
.rd-run { font: 11px ui-monospace, monospace; color: #6b7280; }
.rd-body { max-height: 78vh; overflow-y: auto; }
.rd-bad { color: #fca5a5; }
.rd-md { font-size: 13px; line-height: 1.5; color: #d1d5db; }
.rd-md :deep(h1) { font-size: 18px; font-weight: 500; margin: 0 0 10px; line-height: 1.3; }
.rd-md :deep(h2) { font-size: 14px; font-weight: 500; margin: 18px 0 6px; line-height: 1.3; }
.rd-md :deep(p) { margin: 0 0 10px; }
.rd-md :deep(table) { border-collapse: collapse; margin: 0 0 12px; font-size: 12px; }
.rd-md :deep(th), .rd-md :deep(td) { border-bottom: 1px solid #262c35; padding: 3px 12px 3px 0; text-align: left; }
.rd-md :deep(th) { color: #6b7280; font-weight: 500; }
.rd-md :deep(code) { font: 12px ui-monospace, monospace; color: #c7ccd4; }
.rd-md :deep(strong) { color: #fbbf24; font-weight: 500; }
</style>
