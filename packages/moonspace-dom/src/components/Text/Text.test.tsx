import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Text } from './Text.tsx'

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

test('renders a span by default, with the fg role', () => {
  render(<Text>deploy</Text>, host)
  const el = host.querySelector('span')!
  expect(label(el)).toBe('deploy')
  expect(el.style.getPropertyValue('--ms-text-color')).toBe('var(--fg)')
  expect(el.dataset['weight']).toBe('regular')
})

test('as changes the element without changing the styling', () => {
  render(<Text as="h1">title</Text>, host)
  const el = host.querySelector('h1')!
  expect(el).not.toBeNull()
  // The same hoisted stylesheet, on a different tag.
  expect(el.querySelector('style')!.textContent).toContain('--ms-text-color')
})

test('the colour role resolves to a custom property, not a hex', () => {
  render(<Text color="danger">failed</Text>, host)
  const el = host.querySelector('span')!
  expect(el.style.getPropertyValue('--ms-text-color')).toBe('var(--danger)')
  // A hex here would mean the component had forked from the theme.
  expect(el.getAttribute('style')).not.toMatch(/#[0-9a-f]{6}/i)
})

test('inverse paints the role as background and reads bg for the text', () => {
  render(<Text color="accent" inverse>selected</Text>, host)
  const el = host.querySelector('span')!
  expect(el.dataset['inverse']).toBe('true')
  const sheet = el.querySelector('style')!.textContent!
  expect(sheet).toContain('background:var(--ms-text-color)')
  expect(sheet).toContain('color:var(--bg)')
  // One property drives both the normal and the inverted case.
  expect(el.style.getPropertyValue('--ms-text-color')).toBe('var(--accent)')
})

test('modifiers are data attributes, so one stylesheet serves every combination', () => {
  render([Text({ caps: true, children: 'a' }), Text({ truncate: true, children: 'b' })], host)
  const [first, second] = [...host.querySelectorAll('span')]
  expect(first!.dataset['caps']).toBe('true')
  expect(second!.dataset['truncate']).toBe('true')

  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(new Set(sheets).size).toBe(1)
})

test('the weight declarations are unitless, so the browser keeps them', () => {
  render(<Text weight="bold">bold</Text>, host)
  const sheet = host.querySelector('style')!.textContent!

  /*
   * The regression this pins is silent in every other way. `fontWeight` is not
   * in the compiler's unitless set, so a bare `400` serialises as
   * `font-weight:400px`, which browsers drop outright — the prop typechecks,
   * renders, and does nothing. Only the compiled text shows it.
   */
  expect(sheet).toContain('font-weight:400;')
  expect(sheet).toContain('font-weight:700;')
  expect(sheet).not.toContain('px;')
})

test('a caller style prop survives alongside the colour property', () => {
  render(<Text style={{ width: '40ch' }}>wide</Text>, host)
  const el = host.querySelector('span')!
  expect(el.style.width).toBe('40ch')
  expect(el.style.getPropertyValue('--ms-text-color')).toBe('var(--fg)')
})
