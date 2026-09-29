<template>
  <div v-show="visible" ref="root" class="fw" :style="style" @pointerdown.capture="raise">
    <div class="fw-bar" @pointerdown.prevent="startDrag">
      <div class="fw-close" @pointerdown.stop @click="$emit('update:visible', false)" />
      <span class="fw-title">{{ title }}</span>
      <div class="fw-bar-right" @pointerdown.stop>
        <slot name="titlebar-right" />
      </div>
    </div>
    <div ref="body" class="fw-body" :class="{ 'fw-body-flush': flush, 'fw-body-held': moving }">
      <slot :size="size" />
    </div>
    <div class="fw-grip fw-grip-e" @pointerdown.prevent="startResize('e', $event)" />
    <div class="fw-grip fw-grip-s" @pointerdown.prevent="startResize('s', $event)" />
    <div class="fw-grip fw-grip-se" @pointerdown.prevent="startResize('se', $event)" />
  </div>
</template>

<script lang="ts">
/* Every window takes the next number off one counter when it is raised, so the
 * last one touched is on top. */
let zCounter = 1000
</script>

<script setup lang="ts">
/* A window over the map: a title bar to drag it by and to close it from, a
 * grip on its right and bottom edges and its corner, and its body for the
 * slot, which is told the body's size in pixels.
 *
 * Geometry is in percent of the parent, so a window stays where it was put
 * relative to the page when the browser is resized, and it is kept per `id`
 * in localStorage, so a console reopened after a reload comes back where it
 * was. Storage that throws (a private window) leaves the default geometry.
 * While a window is dragged or resized its body takes no pointer events, so
 * a frame inside it cannot swallow the gesture. */
import { computed, onMounted, onUnmounted, reactive, ref } from 'vue'

interface Geom { x: number; y: number; w: number; h: number }

const props = withDefaults(defineProps<{
  id: string
  title: string
  visible: boolean
  defaultGeom?: Geom
  minSize?: { w: number; h: number }
  /** No inner padding: the content takes the whole body. */
  flush?: boolean
}>(), {
  defaultGeom: () => ({ x: 25, y: 25, w: 50, h: 50 }),
  minSize: () => ({ w: 10, h: 8 }),
  flush: false,
})
defineEmits<{ 'update:visible': [value: boolean] }>()

const root = ref<HTMLDivElement>()
const body = ref<HTMLDivElement>()
const geom = reactive<Geom>({ ...props.defaultGeom })
const z = ref(++zCounter)
const size = reactive({ w: 0, h: 0 })
const moving = ref(false)
const KEY = `simesh.window.${props.id}`

const style = computed(() => ({
  left: `${geom.x}%`, top: `${geom.y}%`, width: `${geom.w}%`, height: `${geom.h}%`, zIndex: z.value,
}))

function raise() { if (z.value !== zCounter) z.value = ++zCounter }

function parentSize() {
  const parent = root.value?.parentElement
  return { w: parent?.clientWidth || 1, h: parent?.clientHeight || 1 }
}

function clamp() {
  geom.w = Math.min(100, Math.max(props.minSize.w, geom.w))
  geom.h = Math.min(100, Math.max(props.minSize.h, geom.h))
  geom.x = Math.min(100 - geom.w, Math.max(0, geom.x))
  geom.y = Math.min(100 - geom.h, Math.max(0, geom.y))
}

function save() {
  try { localStorage.setItem(KEY, JSON.stringify(geom)) } catch { /* private window */ }
}

function load() {
  try {
    const held = JSON.parse(localStorage.getItem(KEY) ?? 'null') as Partial<Geom> | null
    if (held) for (const k of ['x', 'y', 'w', 'h'] as const) {
      if (typeof held[k] === 'number') geom[k] = held[k]
    }
  } catch { /* nothing kept, or not readable */ }
  clamp()
}

/* One pointer gesture at a time, followed on the window rather than the
 * element, so a fast drag that leaves the bar does not drop it. */
type Edge = 'e' | 's' | 'se'
let gesture: { kind: 'drag' | Edge; x: number; y: number; start: Geom } | null = null

function begin(kind: 'drag' | Edge, event: PointerEvent) {
  if (!event.isPrimary) return
  raise()
  gesture = { kind, x: event.clientX, y: event.clientY, start: { ...geom } }
  moving.value = true
  window.addEventListener('pointermove', follow)
  window.addEventListener('pointerup', end)
  window.addEventListener('pointercancel', end)
}

function startDrag(event: PointerEvent) { begin('drag', event) }
function startResize(edge: Edge, event: PointerEvent) { begin(edge, event) }

function follow(event: PointerEvent) {
  if (!gesture) return
  const { w, h } = parentSize()
  const dx = (event.clientX - gesture.x) / w * 100
  const dy = (event.clientY - gesture.y) / h * 100
  const s = gesture.start
  if (gesture.kind === 'drag') {
    geom.x = s.x + dx
    geom.y = s.y + dy
  } else {
    if (gesture.kind.includes('e')) geom.w = Math.min(100 - s.x, s.w + dx)
    if (gesture.kind.includes('s')) geom.h = Math.min(100 - s.y, s.h + dy)
  }
  clamp()
}

function end() {
  gesture = null
  moving.value = false
  window.removeEventListener('pointermove', follow)
  window.removeEventListener('pointerup', end)
  window.removeEventListener('pointercancel', end)
  save()
}

let observer: ResizeObserver | null = null

onMounted(() => {
  load()
  observer = new ResizeObserver(() => {
    size.w = body.value?.clientWidth ?? 0
    size.h = body.value?.clientHeight ?? 0
  })
  if (body.value) observer.observe(body.value)
})

onUnmounted(() => {
  end()
  observer?.disconnect()
})
</script>

<style scoped>
.fw {
  position: absolute;
  display: flex;
  flex-direction: column;
  background: #000;
  border: 1px solid #000;
  box-shadow: 0 0 0 1px #3b4351, 0 8px 24px rgba(0, 0, 0, 0.5);
  border-radius: 6px;
}
.fw-bar {
  display: flex;
  align-items: center;
  height: 28px;
  flex: none;
  padding: 0 10px;
  background: #282828;
  border-radius: 5px 5px 0 0;
  cursor: grab;
  user-select: none;
  touch-action: none;
}
.fw-bar:active { cursor: grabbing; }
.fw-close {
  width: 12px; height: 12px; border-radius: 50%;
  background: #ff5f57; cursor: pointer; flex: none;
}
.fw-close:hover { background: #ff3b30; }
.fw-title {
  flex: 1; text-align: center; font-size: 12px; font-weight: 500;
  color: rgba(255, 255, 255, 0.7);
}
.fw-bar-right { display: flex; gap: 4px; flex-shrink: 0; }
.fw-bar-right :slotted(.fw-zoom-btn) {
  width: 18px; height: 18px; display: flex; align-items: center; justify-content: center;
  border-radius: 4px; font-size: 14px; font-weight: 700;
  color: rgba(255, 255, 255, 0.5); cursor: pointer; font-family: system-ui; line-height: 1;
}
.fw-bar-right :slotted(.fw-zoom-btn:hover) { color: rgba(255, 255, 255, 0.9); background: rgba(255, 255, 255, 0.1); }
.fw-body { flex: 1; min-height: 0; overflow: hidden; padding: 0 5px; border-radius: 0 0 5px 5px; }
.fw-body-flush { padding: 0; }
.fw-body-held { pointer-events: none; }
.fw-grip { position: absolute; z-index: 2; touch-action: none; }
.fw-grip-e { right: -6px; top: 28px; bottom: 12px; width: 12px; cursor: e-resize; }
.fw-grip-s { bottom: -6px; left: 12px; right: 12px; height: 12px; cursor: s-resize; }
.fw-grip-se { right: -8px; bottom: -8px; width: 20px; height: 20px; cursor: se-resize; }
</style>
