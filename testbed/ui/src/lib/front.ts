/* The front's own verbs, asked over the page's one socket.
 *
 * Every editor verb (device_*, geodata_*, nodeset_*, script_*, snapshot_list,
 * losses_compute, coverage) is answered to the asking socket as
 * `{type: <verb>, ok, …}`, in the order asked, per verb; the socket store
 * hands each such answer to `answered`, which settles the oldest request for
 * that verb. None of them needs a simulation running. Imports are HTTP POSTs
 * of the zip, answered the same way. */
import { useSocket } from '../stores/socket'

export type Reply = Record<string, unknown> & { ok: boolean; error?: string }

const waiting = new Map<string, ((reply: Reply) => void)[]>()

/** One of the front's verbs, and its answer. */
export function request(verb: string, fields: Record<string, unknown> = {}): Promise<Reply> {
  return new Promise((resolve) => {
    const socket = useSocket()
    if (!socket.connected) { resolve({ ok: false, error: 'not connected to the front' }); return }
    const queue = waiting.get(verb) ?? []
    queue.push(resolve)
    waiting.set(verb, queue)
    socket.send({ type: verb, ...fields })
  })
}

/** An answer to one of this page's requests: true when it was one. */
export function answered(msg: Record<string, unknown>): boolean {
  const queue = waiting.get(msg.type as string)
  if (!queue?.length || typeof msg.ok !== 'boolean' || msg.sim !== undefined) return false
  queue.shift()!(msg as Reply)
  return true
}

/** A zip POSTed to one of the front's import endpoints. */
export async function upload(path: string, name: string, file: File): Promise<Reply> {
  try {
    const r = await fetch(`${path}?name=${encodeURIComponent(name)}`, { method: 'POST', body: file })
    if (!r.ok) return { ok: false, error: `${r.status} ${await r.text()}` }
    return await r.json() as Reply
  } catch (e) {
    return { ok: false, error: (e as Error).message }
  }
}
