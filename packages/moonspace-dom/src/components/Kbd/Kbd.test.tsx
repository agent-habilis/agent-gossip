import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Kbd } from './Kbd.tsx'

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

test('renders a kbd carrying its label', () => {
  render(<Kbd>⌘K</Kbd>, host)
  const el = host.querySelector('kbd')!
  expect(el).not.toBeNull()
  expect(label(el)).toBe('⌘K')
})

test('the scoped style is the first child', () => {
  render(<Kbd>Esc</Kbd>, host)
  const el = host.querySelector('kbd')!
  // Position is the scoping mechanism, and first place is what keeps
  // `:first-child` and adjacent-sibling selectors meaning what they say.
  expect(el.firstElementChild!.tagName).toBe('STYLE')
})

test('reverse video by colour roles, not by a raised-keycap treatment', () => {
  render(<Kbd>Esc</Kbd>, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('background:var(--bg-raised)')
  expect(sheet).toContain('color:var(--fg)')
  // A keycap would need these; a terminal has neither.
  expect(sheet).not.toContain('border-radius')
  expect(sheet).not.toContain('box-shadow')
})

test('one row tall, one cell of padding each side', () => {
  render(<Kbd>Esc</Kbd>, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('height:calc(1 * var(--ms-row))')
  expect(sheet).toContain('line-height:calc(1 * var(--ms-row))')
  // The label plus exactly two cells, so a row of hints stays on the grid.
  expect(sheet).toContain('padding-inline:1ch')
})

test('attributes pass through to the element', () => {
  render(<Kbd class="hint" attrs={{ 'aria-hidden': 'true' }} />, host)
  const el = host.querySelector('kbd')!
  expect(el.className).toBe('hint')
  expect(el.getAttribute('aria-hidden')).toBe('true')
})

test('two instances share one compiled stylesheet', () => {
  render([Kbd({ children: '⌘K' }), Kbd({ children: 'Esc' })], host)
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)
})
