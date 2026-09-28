/* The little of a zip the page reads before sending it: the name the zip
 * gives its geodata, so the import dialog can offer it.
 *
 * A zip's central directory is at its end, so only the tail and the one small
 * member asked for are read from the file, whatever its size. A SIMesh
 * geodata pack gives its name in `geodata.yaml`'s first line, `# geodata
 * <name>`; a bare planner pack in its manifest's `name`, at the top or inside
 * one directory, made into a usable name as the front makes one. */

interface Entry { name: string; method: number; size: number; offset: number }

const EOCD = 0x06054b50
const EOCD64_LOCATOR = 0x07064b50
const CENTRAL = 0x02014b50
const ZIP64_EXTRA = 0x0001
const MAX_TAIL = 22 + 0xffff + 20
const MAX_MEMBER = 1 << 20

async function view(file: Blob, start: number, end: number): Promise<DataView> {
  return new DataView(await file.slice(start, end).arrayBuffer())
}

function u64(d: DataView, at: number): number {
  return d.getUint32(at, true) + d.getUint32(at + 4, true) * 2 ** 32
}

async function entries(file: File): Promise<Entry[]> {
  const tailStart = Math.max(0, file.size - MAX_TAIL)
  const tail = await view(file, tailStart, file.size)
  let eocd = -1
  for (let i = tail.byteLength - 22; i >= 0; i--) {
    if (tail.getUint32(i, true) === EOCD) { eocd = i; break }
  }
  if (eocd < 0) return []
  let size = tail.getUint32(eocd + 12, true)
  let start = tail.getUint32(eocd + 16, true)
  if ((start === 0xffffffff || size === 0xffffffff) && eocd >= 20
      && tail.getUint32(eocd - 20, true) === EOCD64_LOCATOR) {
    const at = u64(tail, eocd - 12)
    const record = await view(file, at, at + 56)
    size = u64(record, 40)
    start = u64(record, 48)
  }
  const dir = await view(file, start, start + size)
  const out: Entry[] = []
  const decoder = new TextDecoder()
  for (let p = 0; p + 46 <= dir.byteLength && dir.getUint32(p, true) === CENTRAL;) {
    const nameLen = dir.getUint16(p + 28, true)
    const extraLen = dir.getUint16(p + 30, true)
    const commentLen = dir.getUint16(p + 32, true)
    let compressed = dir.getUint32(p + 20, true)
    const plain = dir.getUint32(p + 24, true)
    let offset = dir.getUint32(p + 42, true)
    const name = decoder.decode(new Uint8Array(dir.buffer, dir.byteOffset + p + 46, nameLen))
    for (let x = p + 46 + nameLen; x + 4 <= p + 46 + nameLen + extraLen;) {
      const id = dir.getUint16(x, true), len = dir.getUint16(x + 2, true)
      if (id === ZIP64_EXTRA) {
        let f = x + 4
        if (plain === 0xffffffff) f += 8
        if (compressed === 0xffffffff) { compressed = u64(dir, f); f += 8 }
        if (offset === 0xffffffff) offset = u64(dir, f)
      }
      x += 4 + len
    }
    out.push({ name, method: dir.getUint16(p + 10, true), size: compressed, offset })
    p += 46 + nameLen + extraLen + commentLen
  }
  return out
}

async function text(file: File, e: Entry): Promise<string | null> {
  if (e.size > MAX_MEMBER) return null
  const local = await view(file, e.offset, e.offset + 30)
  const data = e.offset + 30 + local.getUint16(26, true) + local.getUint16(28, true)
  const raw = file.slice(data, data + e.size)
  if (e.method === 0) return await raw.text()
  if (e.method !== 8) return null
  const stream = raw.stream().pipeThrough(new DecompressionStream('deflate-raw'))
  return await new Response(stream).text()
}

/** The nearest usable name to some free text, as the front's `store.slug`. */
export function slug(s: string): string {
  const out = s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '')
    .slice(0, 32).replace(/-+$/, '')
  return /^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$/.test(out) ? out : ''
}

/** The geodata name a zip gives, or '' when it gives none (or is no zip). */
export async function zipGeodataName(file: File): Promise<string> {
  try {
    const all = await entries(file)
    const yaml = all.find(e => e.name === 'geodata.yaml')
    if (yaml) {
      const first = (await text(file, yaml))?.split('\n', 1)[0] ?? ''
      return /^#\s*geodata\s+([a-z0-9-]+)\s*$/.exec(first)?.[1] ?? ''
    }
    const manifest = all.find(e => e.name === 'manifest.json')
      ?? all.find(e => /^[^/]+\/manifest\.json$/.test(e.name))
    if (!manifest) return ''
    const parsed = JSON.parse((await text(file, manifest)) ?? '{}') as { name?: unknown }
    return typeof parsed.name === 'string' ? slug(parsed.name) : ''
  } catch {
    return ''
  }
}
