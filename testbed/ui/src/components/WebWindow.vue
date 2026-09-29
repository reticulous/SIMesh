<template>
  <FloatingWindow
    :id="`sim-web-${name}`"
    :title="`web-ui for ${name}`"
    :visible="visible"
    :default-geom="{ x: 30, y: 8, w: 40, h: 70 }"
    :min-size="{ w: 16, h: 16 }"
    flush
    @update:visible="v => $emit('update:visible', v)"
  >
    <template #titlebar-right>
      <span class="fw-zoom-btn" @click="zoomBy(-1)">-</span>
      <span class="fw-zoom-btn" @click="zoomBy(1)">+</span>
    </template>
    <template #default="{ size }">
      <div class="web-host">
        <iframe :src="src" :title="`web-ui for ${name}`" :style="frameStyle(size)" />
      </div>
    </template>
  </FloatingWindow>
</template>

<script setup lang="ts">
/* A station's web UI in a window over the map, its page in a frame.
 *
 * − and + are the browser's zoom for that page, over the browser's own steps,
 * 75 % to start: the frame is laid out at the window's size divided by the
 * zoom and scaled back to fit, so the page sees the viewport a browser
 * zoomed that far would give it. The zoom is kept, one for every station. */
import { computed, ref } from 'vue'
import FloatingWindow from './FloatingWindow.vue'
import { useSim } from '../stores/sim'

const sim = useSim()
const props = defineProps<{ name: string; visible: boolean }>()
defineEmits<{ 'update:visible': [value: boolean] }>()

const STEPS = [0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2]
const DEFAULT_STEP = STEPS.indexOf(0.75)
const ZOOM_KEY = 'simesh.web.zoom'

function storedStep() {
  try {
    const held = localStorage.getItem(ZOOM_KEY)
    const at = held === null ? -1 : STEPS.indexOf(Number(held))
    return at >= 0 ? at : DEFAULT_STEP
  } catch { return DEFAULT_STEP }
}
const step = ref(storedStep())
const scale = computed(() => STEPS[step.value]!)

function zoomBy(by: number) {
  step.value = Math.min(STEPS.length - 1, Math.max(0, step.value + by))
  try { localStorage.setItem(ZOOM_KEY, String(scale.value)) } catch { /* private window */ }
}

const src = computed(() => sim.stationUrl(props.name))

function frameStyle(size: { w: number; h: number }) {
  return {
    width: `${size.w / scale.value}px`,
    height: `${size.h / scale.value}px`,
    transform: `scale(${scale.value})`,
  }
}
</script>

<style scoped>
.web-host {
  width: 100%;
  height: 100%;
  overflow: hidden;
  background: #fff;
}
.web-host iframe {
  display: block;
  border: 0;
  transform-origin: 0 0;
}
</style>
