import { defineStore } from 'pinia'
import { answered } from '../lib/front'
import { useCatalog } from './catalog'
import { useCoverage } from './coverage'
import { useGeodata } from './geodata'
import { useSim } from './sim'

/* The page's one websocket, to the front (or to a simd on its own), and the
 * stores it feeds. An answer to one of the page's own requests settles it
 * (lib/front); every other message goes to each store's `receive`, which
 * takes what is its own: the listings and script output to `catalog`, a
 * simulation's stream to `sim`, coverage tiles to `coverage`. Reconnecting
 * replays what the other end holds, so the page keeps no state the front
 * cannot restate. */
export const useSocket = defineStore('socket', {
  state: () => ({
    connected: false,
    /** Whether the page is served by the front, and so has the editors. */
    front: false,
    /** The port the browser reached the front on, for the station links. */
    port: String(location.port || '8800'),
    socket: null as WebSocket | null,
  }),

  actions: {
    connect() {
      if (this.socket) return
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
      const socket = new WebSocket(`${proto}//${location.host}/ws`)
      this.socket = socket
      socket.onopen = () => {
        this.connected = true
        useSim().reconnected()
      }
      socket.onclose = () => {
        this.connected = false
        this.socket = null
        // The front is the process; until it is back there is nothing to
        // talk to, so keep trying rather than leave a dead page.
        setTimeout(() => this.connect(), 1000)
      }
      socket.onmessage = (event) => this.receive(JSON.parse(event.data as string) as Record<string, unknown>)
    },

    receive(msg: Record<string, unknown>) {
      if (msg.type === 'hello') {
        // A page older (or newer) than the build the front serves reloads
        // itself, once per build, so a cache handing back the old one cannot loop.
        const mine = document.querySelector('script[type="module"][src*="/assets/"]')?.getAttribute('src')
        const page = typeof msg.page === 'string' ? msg.page : null
        if (mine && page && mine !== page) {
          let tried = false
          try { tried = sessionStorage.getItem('simesh.reloaded') === page; sessionStorage.setItem('simesh.reloaded', page) } catch { /* private window */ }
          if (!tried) { location.reload(); return }
        }
        // Only the front has the editors: a simd on its own answers none of them.
        this.front = true
        this.port = String(msg.port ?? this.port)
        useGeodata().reconnected()
        void useCatalog().refresh()
      }
      if (answered(msg)) return
      useCatalog().receive(msg)
      useCoverage().receive(msg)
      useSim().receive(msg)
    },

    send(msg: Record<string, unknown>) {
      if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify(msg))
    },
  },
})
