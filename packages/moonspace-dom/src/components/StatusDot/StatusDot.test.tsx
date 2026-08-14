import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { StatusDot } from './StatusDot.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** The rendered text, excluding the scoped <style> element's own contents. */
const label = (el: Element): string =>
  [...el.childNodes]
    .filter((n) => n.nodeName !== 'STYLE')
    .map((n) => n.textContent)
    .join('')

const root = (): HTMLElement => host.querySelector('span')!
const part = (name: string): HTMLElement => host.querySelector(`[data-part="${name}"]`)!

test('renders the glyph for the status, in a one-cell marker', () => {
  render(<StatusDot status="ready" />, host)
  expect(part('marker').textContent).toBe('●')
  expect(root().dataset['status']).toBe('ready')
})

test('every status has its own glyph, so colour is never the only signal', () => {
  const glyphs = (['ready', 'building', 'error', 'queued', 'canceled'] as const).map((status) => {
    document.body.innerHTML = ''
    const el = document.createElement('div')
    document.body.append(el)
    render(<StatusDot status={status} />, el)
    return el.querySelector('[data-part="marker"]')!.textContent
  })

  // queued/canceled are the case that matters: fgMuted and fgSubtle both
  // collapse to brightBlack in ANSI-16, so the shapes are all that is left.
  expect(new Set(glyphs).size).toBe(5)
})

test('the status colour is a custom property, resolved from the role', () => {
  render(<StatusDot status="error" />, host)
  expect(root().style.getPropertyValue('--ms-status-color')).toBe('var(--danger)')
  // A hex here would mean the component had forked from the theme.
  expect(root().getAttribute('style')).not.toMatch(/#[0-9a-f]{6}/i)
})

test('without children the status word is the accessible name, visually hidden', () => {
  render(<StatusDot status="building" />, host)
  const hidden = part('sr-label')
  expect(hidden.textContent).toBe('building')

  const sheet = root().querySelector('style')!.textContent!
  expect(sheet).toContain('[data-part="sr-label"]{position:absolute;')
  expect(sheet).toContain('clip-path:inset(50%)')
})

test('children replace the hidden label and the marker stays out of the tree', () => {
  render(<StatusDot status="ready">api-gateway</StatusDot>, host)
  expect(label(root())).toBe('●api-gateway')
  expect(host.querySelector('[data-part="sr-label"]')).toBeNull()
  expect(part('marker').getAttribute('aria-hidden')).toBe('true')
})

test('one stylesheet serves every status', () => {
  render([StatusDot({ status: 'ready' }), StatusDot({ status: 'error' })], host)
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)

  // The sheet is status-agnostic: the colour arrives per instance.
  expect(sheets[0]).toContain('color:var(--ms-status-color)')
  expect(sheets[0]).not.toContain('var(--danger)')
})

test('a caller style prop survives alongside the status colour', () => {
  render(<StatusDot status="queued" style={{ marginLeft: '2ch' }} />, host)
  expect(root().style.marginLeft).toBe('2ch')
  expect(root().style.getPropertyValue('--ms-status-color')).toBe('var(--fg-muted)')
})
