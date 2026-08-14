import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Note } from './Note.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

const root = (): HTMLElement => host.querySelector('div')!
const part = (name: string): HTMLElement => host.querySelector(`[data-part="${name}"]`)!

test('renders a div, defaults to info, and carries its children', () => {
  render(<Note>check the logs</Note>, host)
  expect(root().tagName).toBe('DIV')
  expect(root().dataset['tone']).toBe('info')
  expect(part('marker').textContent).toBe('i')
  expect(part('label').textContent).toBe('note')
  expect(part('body').textContent).toBe('check the logs')
})

test('each tone brings a marker and a word, not just a colour', () => {
  const seen = (['info', 'success', 'warning', 'danger'] as const).map((tone) => {
    document.body.innerHTML = ''
    const el = document.createElement('div')
    document.body.append(el)
    render(<Note tone={tone}>body</Note>, el)
    return [
      el.querySelector('[data-part="marker"]')!.textContent,
      el.querySelector('[data-part="label"]')!.textContent,
    ].join(' ')
  })

  expect(seen).toEqual(['i note', '✓ ok', '! warn', '✗ error'])
})

test('the tone selects in the sheet, through one custom property', () => {
  render(<Note tone="danger">gone</Note>, host)
  expect(root().dataset['tone']).toBe('danger')

  const sheet = root().querySelector('style')!.textContent!
  expect(sheet).toContain('[data-tone="danger"]{--ms-note-color:var(--danger);}')
  expect(sheet).toContain('[data-tone="warning"]{--ms-note-color:var(--warning);}')
  // The parts never learn which tone they are painting.
  expect(sheet).toContain('color:var(--ms-note-color)')
  expect(sheet).toContain('inset var(--ms-border-width) 0 0 0 var(--ms-note-color)')

  // Enumerable variants stay in the sheet — nothing per-instance is inlined.
  expect(root().getAttribute('style')).toBeNull()
})

test('label overrides the default word for the tone', () => {
  render(<Note tone="warning" label="deprecated">use the new flag</Note>, host)
  expect(part('label').textContent).toBe('deprecated')
  // The marker is unaffected — it is the tone's, not the label's.
  expect(part('marker').textContent).toBe('!')
})

test('the rule is a box-shadow, so the note costs no extra rows', () => {
  render(<Note>a</Note>, host)
  const sheet = root().querySelector('style')!.textContent!
  expect(sheet).toContain('padding-inline:1ch')
  expect(sheet).not.toContain('padding-block')
  expect(sheet).not.toContain('border-left')
})

test('one stylesheet serves every tone', () => {
  render([Note({ tone: 'info', children: 'a' }), Note({ tone: 'danger', children: 'b' })], host)
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)
})
