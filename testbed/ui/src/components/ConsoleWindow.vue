<template>
  <FloatingWindow
    :id="`sim-console-${name}`"
    :title="`${name} console`"
    :visible="visible"
    :default-geom="{ x: 18, y: 18, w: 34, h: 30 }"
    :min-size="{ w: 16, h: 12 }"
    flush
    @update:visible="v => $emit('update:visible', v)"
  >
    <template #titlebar-right>
      <span class="fw-zoom-btn" @click="zoomBy(-1)">-</span>
      <span class="fw-zoom-btn" @click="zoomBy(1)">+</span>
    </template>
    <template #default="{ size }">
      <div ref="host" class="console-host" :data-w="size.w" :data-h="size.h" />
    </template>
  </FloatingWindow>
</template>

<script setup lang="ts">
/* A station's serial console: its pty, bridged over a websocket, in an xterm.
 *
 * Keystrokes go out as binary frames and land on the pty exactly as a cable
 * would deliver them, so nothing a person can type is special to the
 * transport; the terminal's size goes the other way as a JSON text frame.
 * − and + step the font by 2 px, from 8 px up, as spangap's CLI window does;
 * the step is kept, one for every console. */
import { ref, watch, onUnmounted, nextTick } from 'vue'
import FloatingWindow from './FloatingWindow.vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { useSim } from '../stores/sim'

const sim = useSim()
const props = defineProps<{ name: string; visible: boolean }>()
defineEmits<{ 'update:visible': [value: boolean] }>()

const host = ref<HTMLDivElement>()
let term: Terminal | null = null
let fit: FitAddon | null = null
let socket: WebSocket | null = null
let observer: ResizeObserver | null = null

const BASE_FONT = 10
const ZOOM_KEY = 'simesh.console.zoom'
function storedZoom() {
  try { return Number(localStorage.getItem(ZOOM_KEY)) || 0 } catch { return 0 }
}
const zoom = ref(storedZoom())
const fontSize = () => Math.max(8, BASE_FONT + zoom.value * 2)

function zoomBy(step: number) {
  zoom.value = Math.min(10, Math.max(-1, zoom.value + step))
  try { localStorage.setItem(ZOOM_KEY, String(zoom.value)) } catch { /* private window */ }
  if (term) { term.options.fontSize = fontSize(); fit?.fit(); sendSize() }
}

function open() {
  if (term || !host.value) return
  term = new Terminal({
    // The pty is raw, because a station's console is a serial line and the
    // firmware writes bare LFs to it — so the terminal does the LF→CRLF
    // translation an `onlcr` tty would, or every line staircases.
    convertEol: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: fontSize(),
    theme: { background: '#0e1116', foreground: '#d1d5db' },
  })
  fit = new FitAddon()
  term.loadAddon(fit)
  term.open(host.value)
  fit.fit()
  term.focus()      // a console you just opened is one you want to type into

  socket = new WebSocket(sim.consoleUrl(props.name))
  socket.binaryType = 'arraybuffer'
  socket.onopen = () => sendSize()
  socket.onmessage = (event) => {
    if (typeof event.data === 'string') term?.write(event.data)
    else term?.write(new Uint8Array(event.data))
  }
  term.onData((data) => {
    if (socket?.readyState === WebSocket.OPEN) {
      socket.send(new TextEncoder().encode(data))
    }
  })

  observer = new ResizeObserver(() => { fit?.fit(); sendSize() })
  observer.observe(host.value)
}

function sendSize() {
  if (!term || socket?.readyState !== WebSocket.OPEN) return
  socket.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }))
}

function close() {
  observer?.disconnect(); observer = null
  socket?.close(); socket = null
  term?.dispose(); term = null
  fit = null
}

watch(() => props.visible, async (shown) => {
  if (shown) { await nextTick(); open() } else close()
}, { immediate: true })

onUnmounted(close)
</script>

<style scoped>
.console-host {
  width: 100%;
  height: 100%;
  background: #0e1116;
}
</style>
