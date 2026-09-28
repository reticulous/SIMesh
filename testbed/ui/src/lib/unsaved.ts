import type { QVueGlobals } from 'quasar'
import { useNodes } from '../stores/nodes'

/* Leaving the nodeset being edited, by another geodata, another layer, New,
 * Import or another tab: with changes not saved, the user saves them,
 * discards them, or stays where they are. `go` runs once nothing is left
 * unsaved. */
export function whenSaved(quasar: QVueGlobals, go: () => void) {
  const nodes = useNodes()
  if (nodes.attached || !nodes.dirty) { go(); return }
  const name = nodes.nodeset?.name ?? null
  quasar.dialog({
    title: 'Unsaved nodes',
    message: `${name ?? 'The new nodeset'} has changes that have not been saved.`,
    options: {
      type: 'radio', model: 'save',
      items: [
        { label: name ? `Save ${name}` : 'Save it as a new nodeset', value: 'save' },
        { label: 'Discard the changes', value: 'discard' },
      ],
    },
    ok: { label: 'Go on', flat: true, noCaps: true },
    cancel: { label: 'Stay', flat: true, noCaps: true },
    persistent: true,
  }).onOk((choice: string) => {
    if (choice === 'discard') { void nodes.discard().then(go); return }
    if (name) { void nodes.save().then(e => done(quasar, e, go)); return }
    quasar.dialog({
      title: 'Save nodeset as', message: 'Lower-case letters, digits and hyphens.',
      prompt: { model: '', type: 'text' }, cancel: true, persistent: true,
    }).onOk((to: string) => {
      if (to.trim()) void nodes.saveAs(to.trim()).then(e => done(quasar, e, go))
    })
  })
}

function done(quasar: QVueGlobals, error: string | null, go: () => void) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 6000 })
  else go()
}
