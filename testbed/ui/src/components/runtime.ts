/* How a simulation's time and plan read, for the registry's rows and the
 * header alike. T is in microseconds; wall times are seconds since the epoch. */
import type { SimSummary } from '../stores/sim'

/** A wall instant as the local hour and minute, with the day when it is not today. */
export function clockTime(epoch: number): string {
  const at = new Date(epoch * 1000)
  const hm = at.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  return at.toDateString() === new Date().toDateString()
    ? hm : `${at.toLocaleDateString([], { weekday: 'short' })} ${hm}`
}

/** T as a clock: "03:12:45", and from a day on "2d + 03:12". */
export function tText(us: number): string {
  let s = Math.max(0, Math.floor(us / 1e6))
  const d = Math.floor(s / 86400); s -= d * 86400
  const h = Math.floor(s / 3600); s -= h * 3600
  const m = Math.floor(s / 60); s -= m * 60
  const two = (v: number) => String(v).padStart(2, '0')
  return d ? `${d}d + ${two(h)}:${two(m)}` : `${two(h)}:${two(m)}:${two(s)}`
}

/** Simulated time: "T 00:20:34", where a stopped one stopped; null when
 *  unknown, and for a real-time run, whose simulated time is its real time. */
export function simText(s: SimSummary): string | null {
  if (s.mode === 'real' || (!s.mode && s.time === 'real')) return null
  return s.t === null || s.t === undefined ? null : `T ${tText(s.t)}`
}

/** Real time elapsed: since it started, or from start to where it stopped. */
export function realText(s: SimSummary, now = Date.now() / 1000): string | null {
  const started = s.started
  if (!started) return null
  const end = s.ended ?? (s.state === 'running' || s.state === 'starting' ? now : null)
  return end === null ? null : tText(Math.max(0, end - started) * 1e6)
}

/** How fast T runs: "2×", "max ≈140×", "real time"; null for a stopped one. */
export function speedText(s: SimSummary): string | null {
  if (s.state === 'paused' || s.state === 'ended' || s.state === 'exited') return null
  if (s.mode !== 'virtual') return s.mode ? 'real time' : null
  if (s.rate) return `${s.rate}×`
  return `max${s.pace ? ` ≈${s.pace.toFixed(s.pace < 10 ? 1 : 0)}×` : ''}`
}

/** "T 00:20:34 simulated · 00:03:10 real · max ≈6×", for one line. */
export function paceText(s: SimSummary): string {
  const sim = simText(s), real = realText(s), speed = speedText(s)
  return [sim && `${sim} simulated`, real && `${real} real`, speed].filter(Boolean).join(' · ') || '—'
}

/** "traffic 23 of 60 min", with the phase's own units; null with no plan. */
export function phaseText(s: SimSummary): string | null {
  if (!s.plan) return null
  if (!s.phase || s.t === null) return s.done ? 'plan done' : null
  const p = s.phase
  const of = p.until - p.from
  const unit = of >= 7200e6 ? 3600e6 : of >= 120e6 ? 60e6 : 1e6
  const name = unit === 3600e6 ? 'h' : unit === 60e6 ? 'min' : 's'
  const fmt = (v: number) => unit === 3600e6 ? (v / unit).toFixed(1) : Math.round(v / unit)
  return `${p.name} ${fmt(Math.max(0, s.t - p.from))} of ${fmt(of)} ${name}`
}

/** "done about 22:40", or null when the pace is not known yet. */
export function etaText(s: SimSummary): string | null {
  return s.eta ? `done about ${clockTime(s.eta)}` : null
}
